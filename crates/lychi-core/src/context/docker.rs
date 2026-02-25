//! Docker daemon and container detection.
//!
//! Checks if Docker is running and lists active containers.
//! Skips instantly if `/var/run/docker.sock` doesn't exist.

use std::path::Path;
use std::process::Command;

use super::{ContainerInfo, DockerContext};

/// Detect Docker daemon status and running containers.
pub fn detect() -> Option<DockerContext> {
    // Fast path: skip if Docker socket doesn't exist
    if !Path::new("/var/run/docker.sock").exists() {
        return None;
    }

    let output = Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
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

    Some(DockerContext {
        daemon_running: true,
        containers,
    })
}
