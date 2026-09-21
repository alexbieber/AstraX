//! Codex defaults/ranges follow CC Switch 06082e1 AppProxyConfig and its
//! AutoFailoverConfigPanel. Both IPC and the UI validate these settings.

use crate::error::{CodexxError, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub(crate) const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1";
pub(crate) const DEFAULT_LISTEN_PORT: u16 = 15721;
pub(crate) const MAX_QUEUE: usize = 64;
pub(crate) const ROUTE_TOKEN_HEADER: &str = "x-codex-x-route-token";
pub(crate) const ROUTE_GENERATION_HEADER: &str = "x-codex-x-route-generation";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RoutingTuning {
    pub(crate) max_retries: u32,
    pub(crate) streaming_first_byte_timeout: u64,
    pub(crate) streaming_idle_timeout: u64,
    pub(crate) non_streaming_timeout: u64,
    pub(crate) circuit_failure_threshold: u32,
    pub(crate) circuit_success_threshold: u32,
    pub(crate) circuit_timeout_seconds: u64,
    pub(crate) circuit_error_rate_threshold: f64,
    pub(crate) circuit_min_requests: u32,
}

impl Default for RoutingTuning {
    fn default() -> Self {
        Self {
            max_retries: 3,
            streaming_first_byte_timeout: 60,
            streaming_idle_timeout: 120,
            non_streaming_timeout: 600,
            circuit_failure_threshold: 4,
            circuit_success_threshold: 2,
            circuit_timeout_seconds: 60,
            circuit_error_rate_threshold: 0.6,
            circuit_min_requests: 10,
        }
    }
}

impl RoutingTuning {
    pub(crate) fn validate(&self) -> Result<()> {
        for (valid, message) in [
            (self.max_retries <= 10, "最大重试次数须为 0–10"),
            (
                (1..=120).contains(&self.streaming_first_byte_timeout),
                "流式首字节超时须为 1–120 秒",
            ),
            (
                self.streaming_idle_timeout <= 600,
                "流式静默超时须为 0–600 秒，0 表示禁用",
            ),
            (
                (60..=1200).contains(&self.non_streaming_timeout),
                "非流式超时须为 60–1200 秒",
            ),
            (
                (1..=20).contains(&self.circuit_failure_threshold),
                "失败阈值须为 1–20",
            ),
            (
                (1..=10).contains(&self.circuit_success_threshold),
                "恢复成功阈值须为 1–10",
            ),
            (
                self.circuit_timeout_seconds <= 300,
                "恢复等待时间须为 0–300 秒",
            ),
            (
                self.circuit_error_rate_threshold.is_finite()
                    && (0.0..=1.0).contains(&self.circuit_error_rate_threshold),
                "错误率阈值须为 0–100%",
            ),
            (
                (5..=100).contains(&self.circuit_min_requests),
                "最小请求数须为 5–100",
            ),
        ] {
            if !valid {
                return Err(CodexxError::Config(message.into()));
            }
        }
        Ok(())
    }
}

pub(crate) fn listen_ip(address: &str) -> Result<IpAddr> {
    let address = address.trim();
    if address.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    let address = address
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(address);
    address
        .parse()
        .map_err(|_| CodexxError::Config("监听地址须为 IPv4、IPv6 或 localhost".into()))
}

pub(crate) fn client_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    }
}

pub(crate) fn local_url(address: IpAddr, port: u16) -> String {
    match client_ip(address) {
        IpAddr::V4(ip) => format!("http://{ip}:{port}/v1"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}/v1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_defaults_and_valid_boundaries_match_reference() {
        let defaults = RoutingTuning::default();
        defaults.validate().unwrap();
        assert_eq!(
            (
                defaults.max_retries,
                defaults.streaming_first_byte_timeout,
                defaults.streaming_idle_timeout,
                defaults.non_streaming_timeout
            ),
            (3, 60, 120, 600)
        );
        assert_eq!(
            (
                defaults.circuit_failure_threshold,
                defaults.circuit_success_threshold,
                defaults.circuit_timeout_seconds,
                defaults.circuit_min_requests
            ),
            (4, 2, 60, 10)
        );
        assert_eq!(defaults.circuit_error_rate_threshold, 0.6);
        RoutingTuning {
            max_retries: 0,
            streaming_first_byte_timeout: 1,
            streaming_idle_timeout: 0,
            non_streaming_timeout: 60,
            circuit_failure_threshold: 1,
            circuit_success_threshold: 1,
            circuit_timeout_seconds: 0,
            circuit_error_rate_threshold: 0.0,
            circuit_min_requests: 5,
        }
        .validate()
        .unwrap();
        RoutingTuning {
            max_retries: 10,
            streaming_first_byte_timeout: 120,
            streaming_idle_timeout: 600,
            non_streaming_timeout: 1200,
            circuit_failure_threshold: 20,
            circuit_success_threshold: 10,
            circuit_timeout_seconds: 300,
            circuit_error_rate_threshold: 1.0,
            circuit_min_requests: 100,
        }
        .validate()
        .unwrap();
    }
    #[test]
    fn invalid_tuning_is_rejected_in_backend() {
        for invalid in [
            RoutingTuning {
                max_retries: 11,
                ..Default::default()
            },
            RoutingTuning {
                streaming_first_byte_timeout: 0,
                ..Default::default()
            },
            RoutingTuning {
                streaming_first_byte_timeout: 121,
                ..Default::default()
            },
            RoutingTuning {
                streaming_idle_timeout: 601,
                ..Default::default()
            },
            RoutingTuning {
                non_streaming_timeout: 59,
                ..Default::default()
            },
            RoutingTuning {
                non_streaming_timeout: 1201,
                ..Default::default()
            },
            RoutingTuning {
                circuit_failure_threshold: 0,
                ..Default::default()
            },
            RoutingTuning {
                circuit_failure_threshold: 21,
                ..Default::default()
            },
            RoutingTuning {
                circuit_success_threshold: 0,
                ..Default::default()
            },
            RoutingTuning {
                circuit_success_threshold: 11,
                ..Default::default()
            },
            RoutingTuning {
                circuit_timeout_seconds: 301,
                ..Default::default()
            },
            RoutingTuning {
                circuit_error_rate_threshold: f64::NAN,
                ..Default::default()
            },
            RoutingTuning {
                circuit_error_rate_threshold: 1.1,
                ..Default::default()
            },
            RoutingTuning {
                circuit_min_requests: 4,
                ..Default::default()
            },
            RoutingTuning {
                circuit_min_requests: 101,
                ..Default::default()
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
    }
    #[test]
    fn listen_addresses_and_client_urls_support_ipv4_ipv6_and_wildcards() {
        assert_eq!(listen_ip("localhost").unwrap().to_string(), "127.0.0.1");
        assert_eq!(
            local_url(listen_ip("0.0.0.0").unwrap(), 15721),
            "http://127.0.0.1:15721/v1"
        );
        assert_eq!(
            local_url(listen_ip("::").unwrap(), 15721),
            "http://[::1]:15721/v1"
        );
        assert_eq!(
            local_url(listen_ip("[::1]").unwrap(), 15721),
            "http://[::1]:15721/v1"
        );
        for value in [
            "256.0.0.1",
            "host.example",
            "127.0.0.1:15721",
            "::1/path",
            "",
        ] {
            assert!(listen_ip(value).is_err());
        }
    }
}
