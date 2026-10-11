//! FFmpeg-free native Job owner for frozen libtest groups.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(windows))]
    return Err("plugin lab native process supervision is currently Windows-only".into());
    #[cfg(windows)]
    {
        use std::io::{Read, Write};
        let mut arguments = std::env::args_os().skip(1);
        let path = arguments.next().ok_or("expected one test launch-plan path")?;
        if arguments.next().is_some() {
            return Err("expected one test launch-plan path".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
        if bytes.len() > 1_048_576 {
            return Err("test launch plan exceeds one MiB".into());
        }
        let plan: mondrian_validation_launcher::plugin_lab::TestLaunchPlan =
            serde_json::from_slice(&bytes)?;
        let report = mondrian_validation_launcher::plugin_lab::launch(&plan)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&plan.report_path)?;
        output.write_all(&serde_json::to_vec_pretty(&report)?)?;
        output.sync_all()?;
        if report.exit_code != Some(0)
            || report.deadline_exceeded
            || report.descendants_outlived_parent
            || !report.descendants_reaped
            || !report.errors.is_empty()
        {
            return Err("libtest process tree failed; raw owner report retained".into());
        }
        Ok(())
    }
}
