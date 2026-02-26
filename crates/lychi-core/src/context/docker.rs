//! Docker daemon and container detection.
//!
//! Checks if Docker is running and lists active containers.
//! Skips instantly if `/var/run/docker.sock` doesn't exist.

use std::path::Path;
use std::process::Command;

use super::{ContainerInfo, DockerContext};

/// Detect Docker daemon status and running containers.
///
/// Uses a 500ms timeout to avoid blocking when the daemon is unresponsive.
pub fn detect() -> Option<DockerContext> {
    use std::time::{Duration, Instant};

    // Fast path: skip if Docker socket doesn't exist
    if !Path::new("/var/run/docker.sock").exists() {
        return None;
    }

    let mut child = Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!("docker ps timed out");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }

    let stdout = child
        .stdout
        .take()
        .map(|out| {
            use std::io::Read;
            let mut s = String::new();
            let mut reader = out;
            let _ = reader.read_to_string(&mut s);
            s
        })
        .unwrap_or_default();

    let containers: Vec<ContainerInfo> = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '\t').collect();
            if parts.len() == 4 {
                Some(ContainerInfo {
                    id: parts[0].to_string(),
                    name: parts[1].to_string(),
                    image: parts[2].to_string(),
                    status: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Some(DockerContext { containers })
}
