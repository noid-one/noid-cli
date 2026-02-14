use serde::{Deserialize, Serialize};

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
    256
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
        assert_eq!(req.mem_mib, 256);
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
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["command"], serde_json::json!(["ls", "-la"]));
        assert_eq!(json["tty"], false);
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
    fn channel_constants() {
        assert_eq!(CHANNEL_STDOUT, 0x01);
        assert_eq!(CHANNEL_STDERR, 0x02);
        assert_eq!(CHANNEL_STDIN, 0x03);
        assert_eq!(CHANNEL_RESIZE, 0x04);
    }
}
