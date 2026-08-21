use std::{collections::BTreeMap, io, process::Command};

pub const UNIT_NAME: &str = "teslatlas-hub.service";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub unit: &'static str,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
}

impl ServiceStatus {
    pub fn status(&self) -> &'static str {
        if self.active_state == "active" {
            "running"
        } else {
            "stopped"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    fn as_systemctl_verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

pub fn status() -> io::Result<ServiceStatus> {
    status_with_runner(&mut real_systemctl)
}

pub fn apply(action: ServiceAction) -> io::Result<ServiceStatus> {
    apply_with_runner(action, &mut real_systemctl)
}

fn apply_with_runner(
    action: ServiceAction,
    runner: &mut impl FnMut(&[&str]) -> io::Result<SystemctlOutput>,
) -> io::Result<ServiceStatus> {
    let output = runner(&[action.as_systemctl_verb(), UNIT_NAME])?;
    ensure_success(action.as_systemctl_verb(), output)?;
    status_with_runner(runner)
}

fn status_with_runner(
    runner: &mut impl FnMut(&[&str]) -> io::Result<SystemctlOutput>,
) -> io::Result<ServiceStatus> {
    let output = runner(&[
        "show",
        "--no-page",
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        UNIT_NAME,
    ])?;
    ensure_success("show", output.clone())?;
    let fields = output
        .stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    Ok(ServiceStatus {
        unit: UNIT_NAME,
        load_state: required_field(&fields, "LoadState")?,
        active_state: required_field(&fields, "ActiveState")?,
        sub_state: required_field(&fields, "SubState")?,
    })
}

fn required_field(fields: &BTreeMap<String, String>, key: &str) -> io::Result<String> {
    fields
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "systemctl status was incomplete",
            )
        })
}

fn ensure_success(action: &str, output: SystemctlOutput) -> io::Result<()> {
    if output.success {
        return Ok(());
    }
    let detail = output.stderr.trim();
    let detail = (!detail.is_empty())
        .then_some(detail)
        .unwrap_or("systemctl failed");
    Err(io::Error::other(format!(
        "systemctl {action} failed: {detail}"
    )))
}

#[derive(Debug, Clone)]
struct SystemctlOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn real_systemctl(arguments: &[&str]) -> io::Result<SystemctlOutput> {
    let output = Command::new("/bin/systemctl").args(arguments).output()?;
    Ok(SystemctlOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{ServiceAction, SystemctlOutput, apply_with_runner, status_with_runner};

    #[test]
    fn status_parses_systemd_show_output() {
        let mut calls = Vec::new();
        let status = status_with_runner(&mut |arguments| {
            calls.push(arguments.to_vec());
            Ok(SystemctlOutput {
                success: true,
                stdout: "LoadState=loaded\nActiveState=active\nSubState=running\n".to_owned(),
                stderr: String::new(),
            })
        })
        .expect("status");
        assert_eq!(status.status(), "running");
        assert_eq!(status.active_state, "active");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "show");
    }

    #[test]
    fn action_returns_the_follow_up_status() {
        let mut calls = Vec::new();
        let status = apply_with_runner(ServiceAction::Restart, &mut |arguments| {
            calls.push(arguments.to_vec());
            Ok(SystemctlOutput {
                success: true,
                stdout: if arguments[0] == "show" {
                    "LoadState=loaded\nActiveState=active\nSubState=running\n".to_owned()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        })
        .expect("restart");
        assert_eq!(status.status(), "running");
        assert_eq!(calls[0][0], "restart");
        assert_eq!(calls[1][0], "show");
    }

    #[test]
    fn failures_do_not_hide_systemctl_stderr() {
        let error = apply_with_runner(ServiceAction::Start, &mut |_| {
            Ok(SystemctlOutput {
                success: false,
                stdout: String::new(),
                stderr: "unit not found".to_owned(),
            })
        })
        .expect_err("failure");
        assert!(error.to_string().contains("unit not found"));
    }
}
