// Adapted from CC Switch 06082e189d65e6d6dbadc35dacdac1ce6c79d89a,
// src-tauri/src/proxy/circuit_breaker.rs. The state transitions and counters
// retain the upstream behavior; locks are synchronous and logs are omitted.
//
// MIT License
//
// Copyright (c) 2025 Jason Young
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! 熔断器模块
//!
//! 实现熔断器模式，用于防止向不健康的供应商发送请求

use super::config::RoutingTuning;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
// The upstream state machine is retained; Codex-X forwards on bounded blocking
// workers, so its short state locks use std rather than awaiting Tokio locks.
struct RwLock<T>(std::sync::RwLock<T>);
impl<T> RwLock<T> {
    fn new(value: T) -> Self {
        Self(std::sync::RwLock::new(value))
    }
    fn read(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(|error| error.into_inner())
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(|error| error.into_inner())
    }
}

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// 关闭状态 - 正常工作
    Closed,
    /// 打开状态 - 熔断激活，拒绝请求
    Open,
    /// 半开状态 - 尝试恢复，允许部分请求通过
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half_open"),
        }
    }
}

/// 熔断器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerConfig {
    /// 失败阈值 - 连续失败多少次后打开熔断器
    pub failure_threshold: u32,
    /// 成功阈值 - 半开状态下成功多少次后关闭熔断器
    pub success_threshold: u32,
    /// 超时时间 - 熔断器打开后多久尝试半开（秒）
    pub timeout_seconds: u64,
    /// 错误率阈值 - 错误率超过此值时打开熔断器 (0.0-1.0)
    pub error_rate_threshold: f64,
    /// 最小请求数 - 计算错误率前的最小请求数
    pub min_requests: u32,
}

impl From<&RoutingTuning> for CircuitBreakerConfig {
    fn from(config: &RoutingTuning) -> Self {
        Self {
            failure_threshold: config.circuit_failure_threshold,
            success_threshold: config.circuit_success_threshold,
            timeout_seconds: config.circuit_timeout_seconds as u64,
            error_rate_threshold: config.circuit_error_rate_threshold,
            min_requests: config.circuit_min_requests,
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 4,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.6,
            min_requests: 10,
        }
    }
}

/// 熔断器实例
pub struct CircuitBreaker {
    /// 当前状态
    state: Arc<RwLock<CircuitState>>,
    /// 连续失败计数
    consecutive_failures: Arc<AtomicU32>,
    /// 连续成功计数（半开状态）
    consecutive_successes: Arc<AtomicU32>,
    /// 总请求计数
    total_requests: Arc<AtomicU32>,
    /// 失败请求计数
    failed_requests: Arc<AtomicU32>,
    /// 上次打开时间
    last_opened_at: Arc<RwLock<Option<Instant>>>,
    /// 配置（支持热更新）
    config: Arc<RwLock<CircuitBreakerConfig>>,
    /// 半开状态已放行的请求数（用于限流）
    half_open_requests: Arc<AtomicU32>,
}

/// 熔断器放行结果
///
/// `used_half_open_permit` 表示本次放行是否占用了 HalfOpen 探测名额。
/// 调用方应在请求结束后把该值传回 `record_success` / `record_failure` 用于正确释放名额。
#[derive(Debug, Clone, Copy)]
pub struct AllowResult {
    pub allowed: bool,
    pub used_half_open_permit: bool,
}

impl CircuitBreaker {
    /// 创建新的熔断器
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(CircuitState::Closed)),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            consecutive_successes: Arc::new(AtomicU32::new(0)),
            total_requests: Arc::new(AtomicU32::new(0)),
            failed_requests: Arc::new(AtomicU32::new(0)),
            last_opened_at: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(config)),
            half_open_requests: Arc::new(AtomicU32::new(0)),
        }
    }

    /// 更新熔断器配置（热更新，不重置状态）
    pub fn update_config(&self, new_config: CircuitBreakerConfig) {
        *self.config.write() = new_config;
    }

    /// 判断当前 Provider 是否“可被纳入候选链路”
    ///
    /// 这个方法不会占用 HalfOpen 探测名额，仅用于路由选择阶段的“可用性判断”：
    /// - Closed / HalfOpen：可用（返回 true）
    /// - Open：若超时到达则切到 HalfOpen 并返回 true，否则返回 false
    ///
    /// 注意：真正发起请求前仍需调用 `allow_request()` 来获取 HalfOpen 探测名额，
    /// 并在请求结束后通过 `record_success()` / `record_failure()` 释放。
    pub fn is_available(&self) -> bool {
        let state = *self.state.read();
        let config = self.config.read();

        match state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if let Some(opened_at) = *self.last_opened_at.read() {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // 释放读锁再转换状态
                        self.transition_to_half_open();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// 检查是否允许请求通过
    pub fn allow_request(&self) -> AllowResult {
        let state = *self.state.read();

        match state {
            CircuitState::Closed => AllowResult {
                allowed: true,
                used_half_open_permit: false,
            },
            CircuitState::Open => {
                let config = self.config.read();
                // 检查是否应该尝试半开
                if let Some(opened_at) = *self.last_opened_at.read() {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // 释放读锁再转换状态
                        self.transition_to_half_open();

                        // 转换后按当前状态决定是否需要获取 HalfOpen 探测名额
                        let current_state = *self.state.read();
                        return match current_state {
                            CircuitState::Closed => AllowResult {
                                allowed: true,
                                used_half_open_permit: false,
                            },
                            CircuitState::HalfOpen => self.allow_half_open_probe(),
                            CircuitState::Open => AllowResult {
                                allowed: false,
                                used_half_open_permit: false,
                            },
                        };
                    }
                }

                AllowResult {
                    allowed: false,
                    used_half_open_permit: false,
                }
            }
            CircuitState::HalfOpen => self.allow_half_open_probe(),
        }
    }

    /// 记录成功
    pub fn record_success(&self, used_half_open_permit: bool) {
        let state = *self.state.read();
        let config = self.config.read();

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // 重置失败计数
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.total_requests.fetch_add(1, Ordering::SeqCst);

        if state == CircuitState::HalfOpen {
            let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;

            if successes >= config.success_threshold {
                drop(config); // 释放读锁再转换状态
                self.transition_to_closed();
            }
        }
    }

    /// 记录失败
    pub fn record_failure(&self, used_half_open_permit: bool) {
        let state = *self.state.read();
        let config = self.config.read();

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // 更新计数器
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        self.total_requests.fetch_add(1, Ordering::SeqCst);
        self.failed_requests.fetch_add(1, Ordering::SeqCst);

        // 重置成功计数
        self.consecutive_successes.store(0, Ordering::SeqCst);

        // 检查是否应该打开熔断器
        match state {
            CircuitState::HalfOpen => {
                // HalfOpen 状态下失败，立即转为 Open
                drop(config);
                self.transition_to_open();
            }
            CircuitState::Closed => {
                // 检查连续失败次数
                if failures >= config.failure_threshold {
                    drop(config); // 释放读锁再转换状态
                    self.transition_to_open();
                } else {
                    // 检查错误率
                    let total = self.total_requests.load(Ordering::SeqCst);
                    let failed = self.failed_requests.load(Ordering::SeqCst);

                    if total >= config.min_requests {
                        let error_rate = failed as f64 / total as f64;

                        if error_rate >= config.error_rate_threshold {
                            drop(config); // 释放读锁再转换状态
                            self.transition_to_open();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 获取当前状态
    #[allow(dead_code)]
    pub fn get_state(&self) -> CircuitState {
        *self.state.read()
    }

    /// 获取统计信息
    #[allow(dead_code)]
    pub fn get_stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            state: *self.state.read(),
            consecutive_failures: self.consecutive_failures.load(Ordering::SeqCst),
            consecutive_successes: self.consecutive_successes.load(Ordering::SeqCst),
            total_requests: self.total_requests.load(Ordering::SeqCst),
            failed_requests: self.failed_requests.load(Ordering::SeqCst),
        }
    }

    pub(crate) fn cooldown_seconds(&self) -> u64 {
        if *self.state.read() != CircuitState::Open {
            return 0;
        }
        let config = self.config.read();
        self.last_opened_at.read().as_ref().map_or(0, |opened| {
            config
                .timeout_seconds
                .saturating_sub(opened.elapsed().as_secs())
        })
    }

    /// 重置熔断器（手动恢复）
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.transition_to_closed();
    }

    fn allow_half_open_probe(&self) -> AllowResult {
        // 半开状态限流：只允许有限请求通过进行探测
        let max_half_open_requests = 1u32;
        let current = self.half_open_requests.fetch_add(1, Ordering::SeqCst);

        if current < max_half_open_requests {
            AllowResult {
                allowed: true,
                used_half_open_permit: true,
            }
        } else {
            // 超过限额，回退计数，拒绝请求
            self.half_open_requests.fetch_sub(1, Ordering::SeqCst);
            AllowResult {
                allowed: false,
                used_half_open_permit: false,
            }
        }
    }

    /// 仅释放 HalfOpen permit，不影响健康统计
    ///
    /// 用于整流器等场景：请求结果不应计入 Provider 健康度，
    /// 但仍需释放占用的探测名额，避免 HalfOpen 状态卡死
    pub fn release_half_open_permit(&self) {
        let mut current = self.half_open_requests.load(Ordering::SeqCst);
        loop {
            if current == 0 {
                return;
            }

            match self.half_open_requests.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// 转换到打开状态
    fn transition_to_open(&self) {
        *self.state.write() = CircuitState::Open;
        *self.last_opened_at.write() = Some(Instant::now());
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
    }

    /// 转换到半开状态
    fn transition_to_half_open(&self) {
        let mut state = self.state.write();
        if *state != CircuitState::Open {
            return;
        }

        *state = CircuitState::HalfOpen;
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // 重置半开状态的请求限流计数
        self.half_open_requests.store(0, Ordering::SeqCst);
    }

    /// 转换到关闭状态
    fn transition_to_closed(&self) {
        *self.state.write() = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // 重置计数器
        self.total_requests.store(0, Ordering::SeqCst);
        self.failed_requests.store(0, Ordering::SeqCst);
    }
}

/// 熔断器统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub total_requests: u32,
    pub failed_requests: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_closed_to_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // 初始状态应该是关闭
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.allow_request().allowed);

        // 记录 3 次失败
        for _ in 0..3 {
            breaker.record_failure(false);
        }

        // 应该转换到打开状态
        assert_eq!(breaker.get_state(), CircuitState::Open);
        assert!(!breaker.allow_request().allowed);
    }

    #[test]
    fn test_circuit_breaker_half_open_to_closed() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // 打开熔断器
        breaker.record_failure(false);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);

        // 手动转换到半开状态
        breaker.transition_to_half_open();
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        // 记录 2 次成功
        breaker.record_success(false);
        breaker.record_success(false);

        // 应该转换到关闭状态
        assert_eq!(breaker.get_state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_transition_does_not_reset_inflight_permit() {
        let config = CircuitBreakerConfig {
            timeout_seconds: 0,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // 进入 Open，然后由于 timeout_seconds=0，allow_request 会立即切换到 HalfOpen 并占用探测名额
        breaker.transition_to_open();
        let first = breaker.allow_request();
        assert!(first.allowed);
        assert!(first.used_half_open_permit);
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        // 模拟并发下的“重复 HalfOpen 转换调用”，不应重置 in-flight 计数
        breaker.transition_to_half_open();

        // 由于名额仍被占用，第二次请求应被拒绝
        let second = breaker.allow_request();
        assert!(!second.allowed);
        assert!(!second.used_half_open_permit);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // 打开熔断器
        breaker.record_failure(false);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);

        // 重置
        breaker.reset();
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.allow_request().allowed);
    }

    #[test]
    fn error_rate_waits_for_minimum_requests_and_uses_inclusive_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold: 20,
            min_requests: 5,
            error_rate_threshold: 0.6,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config.clone());
        breaker.record_failure(false);
        breaker.record_success(false);
        breaker.record_failure(false);
        breaker.record_success(false);
        assert_eq!(breaker.get_stats().total_requests, 4);
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);
        let higher = CircuitBreaker::new(CircuitBreakerConfig {
            error_rate_threshold: 0.8,
            ..config.clone()
        });
        let minimum = CircuitBreaker::new(CircuitBreakerConfig {
            min_requests: 10,
            ..config
        });
        for breaker in [&higher, &minimum] {
            breaker.record_failure(false);
            breaker.record_success(false);
            breaker.record_failure(false);
            breaker.record_success(false);
            breaker.record_failure(false);
            assert_eq!(breaker.get_state(), CircuitState::Closed);
        }
    }

    #[test]
    fn successes_break_consecutive_failures_without_clearing_error_rate_history() {
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 2,
            min_requests: 100,
            ..Default::default()
        });
        breaker.record_failure(false);
        breaker.record_success(false);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert_eq!(breaker.get_stats().failed_requests, 2);
        assert_eq!(breaker.get_stats().consecutive_failures, 1);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);
        assert_eq!(breaker.get_stats().total_requests, 4);
    }

    #[test]
    fn recovery_wait_and_hot_configuration_preserve_existing_evidence() {
        let mut config = CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config.clone());
        breaker.record_failure(false);
        *breaker.last_opened_at.write() = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(!breaker.allow_request().allowed);
        assert_eq!(breaker.cooldown_seconds(), 1);
        config.timeout_seconds = 1;
        breaker.update_config(config);
        assert_eq!(breaker.get_stats().failed_requests, 1);
        let probe = breaker.allow_request();
        assert!(probe.allowed && probe.used_half_open_permit);
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);
        assert!(!breaker.allow_request().allowed);
        breaker.release_half_open_permit();
        let probe = breaker.allow_request();
        assert!(probe.allowed);
        breaker.record_failure(probe.used_half_open_permit);
        assert_eq!(breaker.get_state(), CircuitState::Open);
        assert!(!breaker.allow_request().allowed);
    }

    #[test]
    fn half_open_requires_configured_successes_and_reset_clears_all_statistics() {
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 3,
            timeout_seconds: 0,
            ..Default::default()
        });
        breaker.record_failure(false);
        for completed in 0..3 {
            let probe = breaker.allow_request();
            assert!(probe.allowed && probe.used_half_open_permit);
            assert!(!breaker.allow_request().allowed);
            breaker.record_success(probe.used_half_open_permit);
            assert_eq!(
                breaker.get_state(),
                if completed < 2 {
                    CircuitState::HalfOpen
                } else {
                    CircuitState::Closed
                }
            );
        }
        assert_eq!(breaker.get_stats().total_requests, 0);
        breaker.record_failure(false);
        breaker.reset();
        let stats = breaker.get_stats();
        assert_eq!(
            (
                stats.total_requests,
                stats.failed_requests,
                stats.consecutive_failures,
                stats.consecutive_successes
            ),
            (0, 0, 0, 0)
        );
        assert!(breaker.allow_request().allowed);
    }
}
