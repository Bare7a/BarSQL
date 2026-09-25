use std::ffi::OsString;
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::process::Command;

// Runs `program` with admin rights behind the OS prompt, waits for it and returns its exit code.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn run(program: &Path, args: &[OsString], _prompt: &str) -> io::Result<i32> {
    let status = Command::new("pkexec").arg(program).args(args).status()?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(target_os = "macos")]
pub fn run(program: &Path, args: &[OsString], prompt: &str) -> io::Result<i32> {
    let output = Command::new("osascript")
        .args(APPLESCRIPT.into_iter().flat_map(|line| ["-e", line]))
        .arg(prompt)
        .arg(program)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let code = stdout.split(['\n', '\r']).rfind(|line| !line.trim().is_empty()).unwrap_or_default();
    code.trim().parse().map_err(|_| io::Error::other(format!("unexpected osascript output: {stdout}")))
}

// A failing `do shell script` hides the exit code, so the command echoes it instead.
#[cfg(target_os = "macos")]
const APPLESCRIPT: [&str; 7] = [
    "on run argv",
    "set command to quoted form of item 2 of argv",
    "repeat with arg in rest of rest of argv",
    "set command to command & \" \" & quoted form of (arg as text)",
    "end repeat",
    "return do shell script (command & \"; echo $?\") with prompt (item 1 of argv) with administrator privileges",
    "end run",
];

#[cfg(windows)]
pub fn run(program: &Path, args: &[OsString], _prompt: &str) -> io::Result<i32> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};

    const SW_HIDE: i32 = 0;
    let wide = |text: &OsStr| text.encode_wide().chain([0]).collect::<Vec<u16>>();
    let params: Vec<String> = args.iter().map(|arg| quote(&arg.to_string_lossy())).collect();
    let (verb, file, params) = (wide(OsStr::new("runas")), wide(program.as_os_str()), wide(params.join(" ").as_ref()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // SAFETY: the strings outlive the call, and the process handle is checked, waited on and closed.
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.hProcess.is_null() {
            return Err(io::Error::other("the elevated process has no handle"));
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let read = GetExitCodeProcess(info.hProcess, &mut code) != 0;
        CloseHandle(info.hProcess);
        if read { Ok(code as i32) } else { Err(io::Error::other("the elevated process has no exit code")) }
    }
}

// Quotes one argument the way CommandLineToArgvW splits it. Backslashes only need doubling before a quote.
#[cfg(any(windows, test))]
fn quote(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn windows_arguments_keep_their_spaces_quotes_and_backslashes() {
        let cases = [
            (r"C:\Program Files\BarSQL\BarSQL.exe", r#""C:\Program Files\BarSQL\BarSQL.exe""#),
            (r"C:\temp dir\", r#""C:\temp dir\\""#),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#""a\\\"b""#),
            ("", r#""""#),
        ];
        for (arg, quoted) in cases {
            assert_eq!(quote(arg), quoted, "{arg}");
        }
    }
}
