use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();

    jira_ops::run_main(
        std::env::args_os().skip(1),
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}
