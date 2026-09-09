//! Native validation bootstrap. This binary deliberately has no FFmpeg dependency.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(windows))]
    return Err("native validation launcher is currently Windows-only".into());
    #[cfg(windows)]
    {
        use std::io::{Read, Write};
        let mut arguments = std::env::args_os().skip(1);
        let path = arguments.next().ok_or("expected one launch-plan JSON path")?;
        if arguments.next().is_some() {
            return Err("expected one launch-plan JSON path".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
        if bytes.len() > 1_048_576 {
            return Err("launch plan exceeds one MiB".into());
        }
        let plan: mondrian_validation_launcher::LaunchPlan = serde_json::from_slice(&bytes)?;
        let report = mondrian_validation_launcher::launch(&plan)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&plan.report_path)?;
        output.write_all(&serde_json::to_vec_pretty(&report)?)?;
        output.sync_all()?;
        if !report.errors.is_empty() || report.exit_code != Some(0) || !report.capsule_removed {
            return Err("launcher or child failed; raw outcome retained in outer receipt".into());
        }
        Ok(())
    }
}
