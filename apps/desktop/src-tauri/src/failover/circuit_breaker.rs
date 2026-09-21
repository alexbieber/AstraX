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

//! Circuit breaker module
//!
//! Implements the circuit breaker pattern to avoid sending requests to unhealthy providers

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

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Closed — operating normally
    Closed,
    /// Open — breaker tripped; reject requests
    Open,
    /// Half-open — probing recovery; allow limited requests
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

/// Circuit breaker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerConfig {
    /// Failure threshold — consecutive failures before opening
    pub failure_threshold: u32,
    /// Success threshold — successes in half-open before closing
    pub success_threshold: u32,
    /// Timeout — seconds after open before trying half-open
    pub timeout_seconds: u64,
    /// Error-rate threshold — open when error rate exceeds this (0.0-1.0)
    pub error_rate_threshold: f64,
    /// Minimum requests before computing error rate
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

/// Circuit breaker instance
pub struct CircuitBreaker {
    /// Current state
    state: Arc<RwLock<CircuitState>>,
    /// Consecutive failure count
    consecutive_failures: Arc<AtomicU32>,
    /// Consecutive success count (half-open)
    consecutive_successes: Arc<AtomicU32>,
    /// Total request count
    total_requests: Arc<AtomicU32>,
    /// Failed request count
    failed_requests: Arc<AtomicU32>,
    /// Last time the breaker opened
    last_opened_at: Arc<RwLock<Option<Instant>>>,
    /// Configuration (supports hot reload)
    config: Arc<RwLock<CircuitBreakerConfig>>,
    /// Requests already allowed in half-open (for rate limiting)
    half_open_requests: Arc<AtomicU32>,
}

/// Circuit breaker allow result
///
/// `used_half_open_permit` indicates whether this allow consumed a half-open probe permit.
/// Callers should pass this back to `record_success` / `record_failure` after the request to release the permit.
#[derive(Debug, Clone, Copy)]
pub struct AllowResult {
    pub allowed: bool,
    pub used_half_open_permit: bool,
}

impl CircuitBreaker {
    /// Create a new circuit breaker
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

    /// Update circuit breaker config (hot reload; does not reset state)
    pub fn update_config(&self, new_config: CircuitBreakerConfig) {
        *self.config.write() = new_config;
    }

    /// Whether the current provider can be included in the candidate chain
    ///
    /// This does not consume a half-open probe permit; it is only an availability check during routing:
    /// - Closed / HalfOpen: available (returns true)
    /// - Open: if the timeout elapsed, switch to HalfOpen and return true; otherwise false
    ///
    /// Note: before sending a request, still call `allow_request()` to obtain a half-open probe permit,
    /// and release it via `record_success()` / `record_failure()` after the request.
    pub fn is_available(&self) -> bool {
        let state = *self.state.read();
        let config = self.config.read();

        match state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if let Some(opened_at) = *self.last_opened_at.read() {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // release read lock before changing state
                        self.transition_to_half_open();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Check whether a request is allowed through
    pub fn allow_request(&self) -> AllowResult {
        let state = *self.state.read();

        match state {
            CircuitState::Closed => AllowResult {
                allowed: true,
                used_half_open_permit: false,
            },
            CircuitState::Open => {
                let config = self.config.read();
                // Check whether we should try half-open
                if let Some(opened_at) = *self.last_opened_at.read() {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // release read lock before changing state
                        self.transition_to_half_open();

                        // After transition, decide from current state whether a half-open probe permit is needed
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

    /// Record a success
    pub fn record_success(&self, used_half_open_permit: bool) {
        let state = *self.state.read();
        let config = self.config.read();

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // Reset failure count
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.total_requests.fetch_add(1, Ordering::SeqCst);

        if state == CircuitState::HalfOpen {
            let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;

            if successes >= config.success_threshold {
                drop(config); // release read lock before changing state
                self.transition_to_closed();
            }
        }
    }

    /// Record a failure
    pub fn record_failure(&self, used_half_open_permit: bool) {
        let state = *self.state.read();
        let config = self.config.read();

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // Update counters
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        self.total_requests.fetch_add(1, Ordering::SeqCst);
        self.failed_requests.fetch_add(1, Ordering::SeqCst);

        // Reset success count
        self.consecutive_successes.store(0, Ordering::SeqCst);

        // Check whether the breaker should open
        match state {
            CircuitState::HalfOpen => {
                // Failure in HalfOpen immediately transitions to Open
                drop(config);
                self.transition_to_open();
            }
            CircuitState::Closed => {
                // Check consecutive failure count
                if failures >= config.failure_threshold {
                    drop(config); // release read lock before changing state
                    self.transition_to_open();
                } else {
                    // Check error rate
                    let total = self.total_requests.load(Ordering::SeqCst);
                    let failed = self.failed_requests.load(Ordering::SeqCst);

                    if total >= config.min_requests {
                        let error_rate = failed as f64 / total as f64;

                        if error_rate >= config.error_rate_threshold {
                            drop(config); // release read lock before changing state
                            self.transition_to_open();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Get the current state
    #[allow(dead_code)]
    pub fn get_state(&self) -> CircuitState {
        *self.state.read()
    }

    /// Get statistics
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

    /// Reset the circuit breaker (manual recovery)
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.transition_to_closed();
    }

    fn allow_half_open_probe(&self) -> AllowResult {
        // Half-open rate limit: only allow a limited number of probe requests
        let max_half_open_requests = 1u32;
        let current = self.half_open_requests.fetch_add(1, Ordering::SeqCst);

        if current < max_half_open_requests {
            AllowResult {
                allowed: true,
                used_half_open_permit: true,
            }
        } else {
            // Over the limit — count rejection and deny the request
            self.half_open_requests.fetch_sub(1, Ordering::SeqCst);
            AllowResult {
                allowed: false,
                used_half_open_permit: false,
            }
        }
    }

    /// Release a half-open permit only; do not affect health stats
    ///
    /// Used for rectifier-like cases where the result should not affect provider health,
    /// but the probe permit must still be released to avoid getting stuck in HalfOpen
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

    /// Transition to Open
    fn transition_to_open(&self) {
        *self.state.write() = CircuitState::Open;
        *self.last_opened_at.write() = Some(Instant::now());
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
    }

    /// Transition to HalfOpen
    fn transition_to_half_open(&self) {
        let mut state = self.state.write();
        if *state != CircuitState::Open {
            return;
        }

        *state = CircuitState::HalfOpen;
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // Reset half-open request rate-limit counter
        self.half_open_requests.store(0, Ordering::SeqCst);
    }

    /// Transition to Closed
    fn transition_to_closed(&self) {
        *self.state.write() = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // Reset counters
        self.total_requests.store(0, Ordering::SeqCst);
        self.failed_requests.store(0, Ordering::SeqCst);
    }
}

/// Circuit breaker statistics
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

        // Initial state should be Closed
        assert_eq!(breaker.get_state(), CircuitState::Closed);
        assert!(breaker.allow_request().allowed);

        // Record 3 failures
        for _ in 0..3 {
            breaker.record_failure(false);
        }

        // Should transition to Open
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

        // Open the circuit breaker
        breaker.record_failure(false);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);

        // Manually transition to HalfOpen
        breaker.transition_to_half_open();
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        // Record 2 successes
        breaker.record_success(false);
        breaker.record_success(false);

        // Should transition to Closed
        assert_eq!(breaker.get_state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_transition_does_not_reset_inflight_permit() {
        let config = CircuitBreakerConfig {
            timeout_seconds: 0,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // Enter Open; with timeout_seconds=0, allow_request immediately switches to HalfOpen and takes a probe permit
        breaker.transition_to_open();
        let first = breaker.allow_request();
        assert!(first.allowed);
        assert!(first.used_half_open_permit);
        assert_eq!(breaker.get_state(), CircuitState::HalfOpen);

        // Simulate concurrent duplicate HalfOpen transitions; in-flight count must not reset
        breaker.transition_to_half_open();

        // Permit still held, so the second request should be denied
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

        // Open the circuit breaker
        breaker.record_failure(false);
        breaker.record_failure(false);
        assert_eq!(breaker.get_state(), CircuitState::Open);

        // Reset
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
