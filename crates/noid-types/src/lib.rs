use serde::{Deserialize, Serialize};

// --- Env var validation ---

/// Maximum number of env vars allowed per request.
pub const MAX_ENV_VARS: usize = 64;

/// Maximum length of a single env var value in bytes.
pub const MAX_ENV_VALUE_LEN: usize = 32 * 1024; // 32 KiB

/// Validate that a string is a legal environment variable name.
/// Accepts `[A-Za-z_][A-Za-z0-9_]*`.
pub fn validate_env_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a slice of `KEY=VALUE` env var strings.
/// Checks format, name validity, count limit, and value size limit.
/// Returns an error message string on failure, Ok(()) on success.
pub fn validate_env_vars(env: &[String]) -> Result<(), String> {
    if env.len() > MAX_ENV_VARS {
        return Err(format!(
            "too many env vars ({}, max {MAX_ENV_VARS})",
            env.len()
        ));
    }
    for e in env {
        let (name, value) = match e.split_once('=') {
            Some(pair) => pair,
            None => return Err(format!("invalid env var (expected KEY=VALUE): {e}")),
        };
        if !validate_env_name(name) {
            return Err(format!("invalid env var name: {name}"));
        }
        if value.len() > MAX_ENV_VALUE_LEN {
            return Err(format!(
                "env var value too long for {name} ({} bytes, max {MAX_ENV_VALUE_LEN})",
                value.len()
            ));
        }
    }
    Ok(())
}

// --- WS channel constants ---

pub const CHANNEL_STDOUT: u8 = 0x01;
pub const CHANNEL_STDERR: u8 = 0x02;
pub const CHANNEL_STDIN: u8 = 0x03;
pub const CHANNEL_RESIZE: u8 = 0x04;

// --- REST request types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateVmRequest {
    pub name: String,
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_mem_mib")]
    pub mem_mib: u32,
}

fn default_cpus() -> u32 {
    1
}
fn default_mem_mib() -> u32 {
    2048
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRequest {
    pub checkpoint_id: String,
    pub new_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: Vec<String>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub env: Vec<String>,
}

// --- REST response types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInfo {
    pub name: String,
    pub state: String,
    pub cpus: u32,
    pub mem_mib: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    pub stdout: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub vm_name: String,
    pub label: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// --- Meta types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub api_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoamiResponse {
    pub user_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub api_version: u32,
    pub max_exec_output_bytes: usize,
    pub exec_timeout_secs: u64,
    pub console_timeout_secs: u64,
    pub max_vm_name_length: usize,
    pub default_cpus: u32,
    pub default_mem_mib: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_vm_request_json() {
        let req = CreateVmRequest {
            name: "test".into(),
            cpus: 2,
            mem_mib: 256,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["cpus"], 2);
        assert_eq!(json["mem_mib"], 256);
    }

    #[test]
    fn create_vm_request_defaults() {
        let json = r#"{"name":"test"}"#;
        let req: CreateVmRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.cpus, 1);
        assert_eq!(req.mem_mib, 2048);
    }

    #[test]
    fn vm_info_json() {
        let info = VmInfo {
            name: "myvm".into(),
            state: "running".into(),
            cpus: 1,
            mem_mib: 128,
            created_at: "2025-01-01 00:00:00".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: VmInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "myvm");
        assert_eq!(parsed.state, "running");
    }

    #[test]
    fn exec_request_json() {
        let req = ExecRequest {
            command: vec!["ls".into(), "-la".into()],
            tty: false,
            env: vec![],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["command"], serde_json::json!(["ls", "-la"]));
        assert_eq!(json["tty"], false);
    }

    #[test]
    fn exec_request_env_round_trip() {
        let req = ExecRequest {
            command: vec!["sh".into(), "-c".into(), "echo $FOO".into()],
            tty: false,
            env: vec!["FOO=bar".into(), "DB_HOST=localhost".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ExecRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.env, vec!["FOO=bar", "DB_HOST=localhost"]);
    }

    #[test]
    fn exec_request_env_backward_compat() {
        // Old clients omitting env field should deserialize to empty vec
        let json = r#"{"command":["ls"],"tty":false}"#;
        let req: ExecRequest = serde_json::from_str(json).unwrap();
        assert!(req.env.is_empty());
    }

    #[test]
    fn exec_result_json() {
        let res = ExecResult {
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
        };
        let json = serde_json::to_string(&res).unwrap();
        let parsed: ExecResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.exit_code, Some(0));
    }

    #[test]
    fn exec_response_json() {
        let res = ExecResponse {
            stdout: "hello\n".into(),
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
        };
        let json = serde_json::to_string(&res).unwrap();
        let parsed: ExecResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.stdout, "hello\n");
    }

    #[test]
    fn checkpoint_info_json() {
        let info = CheckpointInfo {
            id: "abc12345".into(),
            vm_name: "myvm".into(),
            label: Some("before-upgrade".into()),
            created_at: "2025-01-01 00:00:00".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: CheckpointInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc12345");
        assert_eq!(parsed.label, Some("before-upgrade".into()));
    }

    #[test]
    fn checkpoint_info_null_label() {
        let info = CheckpointInfo {
            id: "abc12345".into(),
            vm_name: "myvm".into(),
            label: None,
            created_at: "2025-01-01 00:00:00".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json["label"].is_null());
    }

    #[test]
    fn error_response_json() {
        let err = ErrorResponse {
            error: "not found".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("not found"));
    }

    #[test]
    fn capabilities_json() {
        let caps = Capabilities {
            api_version: 1,
            max_exec_output_bytes: 1048576,
            exec_timeout_secs: 30,
            console_timeout_secs: 3600,
            max_vm_name_length: 64,
            default_cpus: 1,
            default_mem_mib: 256,
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["api_version"], 1);
        assert_eq!(json["max_exec_output_bytes"], 1048576);
    }

    #[test]
    fn whoami_response_json() {
        let who = WhoamiResponse {
            user_id: "uuid-123".into(),
            name: "alice".into(),
        };
        let json = serde_json::to_string(&who).unwrap();
        let parsed: WhoamiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "alice");
    }

    #[test]
    fn version_info_json() {
        let ver = VersionInfo {
            version: "0.1.0".into(),
            api_version: 1,
        };
        let json = serde_json::to_string(&ver).unwrap();
        let parsed: VersionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.api_version, 1);
    }

    #[test]
    fn restore_request_json() {
        let req = RestoreRequest {
            checkpoint_id: "abc12345".into(),
            new_name: Some("restored-vm".into()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["checkpoint_id"], "abc12345");
        assert_eq!(json["new_name"], "restored-vm");
    }

    #[test]
    fn validate_env_name_valid() {
        assert!(validate_env_name("FOO"));
        assert!(validate_env_name("_BAR"));
        assert!(validate_env_name("DB_HOST_1"));
        assert!(validate_env_name("a"));
        assert!(validate_env_name("_"));
    }

    #[test]
    fn validate_env_name_invalid() {
        assert!(!validate_env_name(""));
        assert!(!validate_env_name("1FOO"));
        assert!(!validate_env_name("FOO;rm"));
        assert!(!validate_env_name("FOO BAR"));
        assert!(!validate_env_name("FOO=BAR"));
        assert!(!validate_env_name("a-b"));
        assert!(!validate_env_name("$(cmd)"));
    }

    #[test]
    fn validate_env_vars_valid() {
        let env = vec!["FOO=bar".into(), "DB_HOST=localhost".into()];
        assert!(validate_env_vars(&env).is_ok());
    }

    #[test]
    fn validate_env_vars_empty_value() {
        let env = vec!["FOO=".into()];
        assert!(validate_env_vars(&env).is_ok());
    }

    #[test]
    fn validate_env_vars_missing_equals() {
        let env = vec!["FOO".into()];
        assert!(validate_env_vars(&env).is_err());
    }

    #[test]
    fn validate_env_vars_bad_name() {
        let env = vec!["1BAD=val".into()];
        assert!(validate_env_vars(&env).is_err());
    }

    #[test]
    fn validate_env_vars_too_many() {
        let env: Vec<String> = (0..65).map(|i| format!("V{i}=x")).collect();
        let err = validate_env_vars(&env).unwrap_err();
        assert!(err.contains("too many"));
    }

    #[test]
    fn validate_env_vars_value_too_long() {
        let env = vec![format!("BIG={}", "x".repeat(33 * 1024))];
        let err = validate_env_vars(&env).unwrap_err();
        assert!(err.contains("too long"));
    }

    #[test]
    fn channel_constants() {
        assert_eq!(CHANNEL_STDOUT, 0x01);
        assert_eq!(CHANNEL_STDERR, 0x02);
        assert_eq!(CHANNEL_STDIN, 0x03);
        assert_eq!(CHANNEL_RESIZE, 0x04);
    }
}
