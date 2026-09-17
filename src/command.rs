//! Runs commands whose successful exit status is required.

use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

/// Captures command output, retaining both output streams in failure diagnostics.
pub fn run_command(command: &mut Command, operation: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("run {operation}"))?;
    if !output.status.success() {
        bail!(
            "{operation} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Returns successful output and reports both streams when a command fails.
    fn captures_command_output_and_errors() {
        let output = run_command(Command::new("printf").arg("hello"), "print greeting").unwrap();
        assert_eq!(output.stdout, b"hello");
        let error = run_command(
            Command::new("sh").args(["-c", "printf stdout; printf stderr >&2; exit 1"]),
            "example command",
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "example command failed:\nstdoutstderr");
    }
}
