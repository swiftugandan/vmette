// Minimal libkrun driver.
//
// Boots an in-process microVM (Hypervisor.framework on macOS, KVM on Linux)
// using libkrun's bundled kernel, mounts a host directory as the guest's
// rootfs via virtio-fs, and execs a command inside.
//
// Usage:
//   libkrun-spike --rootfs ./assets/alpine-rootfs --cmd /bin/sh -- -lc 'uname -a; id; cat /etc/os-release'
//
// `krun_start_enter` blocks until the guest exits.

use std::env;
use std::ffi::CString;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr;

#[link(name = "krun", kind = "dylib")]
extern "C" {
    fn krun_set_log_level(level: u32) -> i32;
    fn krun_create_ctx() -> i32;
    fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    fn krun_set_root(ctx_id: u32, root_path: *const c_char) -> i32;
    fn krun_set_workdir(ctx_id: u32, workdir_path: *const c_char) -> i32;
    fn krun_set_exec(
        ctx_id: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;
    fn krun_start_enter(ctx_id: u32) -> i32;
}

struct Args {
    rootfs: PathBuf,
    cmd: String,
    cmd_args: Vec<String>,
    workdir: String,
    vcpus: u8,
    mem_mib: u32,
    log_level: u32,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut rootfs = None;
        let mut cmd = None;
        let mut cmd_args = Vec::new();
        let mut workdir = String::from("/");
        let mut vcpus: u8 = 1;
        let mut mem_mib: u32 = 256;
        let mut log_level: u32 = 1;
        let mut passthrough = false;

        let mut iter = env::args().skip(1);
        while let Some(a) = iter.next() {
            if passthrough {
                cmd_args.push(a);
                continue;
            }
            match a.as_str() {
                "--" => passthrough = true,
                "--rootfs" => rootfs = iter.next().map(PathBuf::from),
                "--cmd" => cmd = iter.next(),
                "--workdir" => workdir = iter.next().ok_or("missing --workdir value")?,
                "--vcpus" => {
                    vcpus = iter
                        .next()
                        .ok_or("missing --vcpus value")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
                "--mem-mib" => {
                    mem_mib = iter
                        .next()
                        .ok_or("missing --mem-mib value")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
                "--log-level" => {
                    log_level = iter
                        .next()
                        .ok_or("missing --log-level value")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown arg: {other}")),
            }
        }

        Ok(Self {
            rootfs: rootfs.ok_or("--rootfs required")?,
            cmd: cmd.ok_or("--cmd required")?,
            cmd_args,
            workdir,
            vcpus,
            mem_mib,
            log_level,
        })
    }
}

fn print_help() {
    eprintln!(
        "libkrun-spike --rootfs PATH --cmd /bin/sh [-- ARG1 ARG2 ...]
  --rootfs    PATH    extracted OCI rootfs directory
  --cmd       PATH    absolute path to executable inside the guest
  [--workdir  PATH    default '/']
  [--vcpus    N       default 1]
  [--mem-mib  N       default 256]
  [--log-level N      libkrun log level 0–5, default 1 (info)]"
    );
}

fn check(label: &str, rc: i32) -> Result<i32, String> {
    if rc < 0 {
        Err(format!("{label} failed (rc={rc}, errno={})", -rc))
    } else {
        Ok(rc)
    }
}

fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n");
            print_help();
            return ExitCode::from(2);
        }
    };

    if let Err(e) = run(args) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run(args: Args) -> Result<(), String> {
    let rootfs = args
        .rootfs
        .canonicalize()
        .map_err(|e| format!("rootfs {}: {}", args.rootfs.display(), e))?;
    if !rootfs.is_dir() {
        return Err(format!("rootfs {} is not a directory", rootfs.display()));
    }

    let rootfs_c = CString::new(rootfs.to_string_lossy().as_bytes()).unwrap();
    let workdir_c = CString::new(args.workdir.as_bytes()).unwrap();
    let cmd_c = CString::new(args.cmd.as_bytes()).unwrap();

    // argv: cmd as argv[0], then user args, NULL terminator.
    let argv_owned: Vec<CString> = std::iter::once(args.cmd.clone())
        .chain(args.cmd_args.iter().cloned())
        .map(|s| CString::new(s).unwrap())
        .collect();
    let mut argv_ptrs: Vec<*const c_char> = argv_owned.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(ptr::null());

    let envp_owned: Vec<CString> = vec![
        CString::new("PATH=/usr/sbin:/usr/bin:/sbin:/bin").unwrap(),
        CString::new("TERM=dumb").unwrap(),
        CString::new("HOME=/root").unwrap(),
    ];
    let mut envp_ptrs: Vec<*const c_char> = envp_owned.iter().map(|c| c.as_ptr()).collect();
    envp_ptrs.push(ptr::null());

    eprintln!(
        "→ rootfs {} | exec {} ({} arg{}) | {} vcpu, {} MiB",
        rootfs.display(),
        args.cmd,
        args.cmd_args.len(),
        if args.cmd_args.len() == 1 { "" } else { "s" },
        args.vcpus,
        args.mem_mib,
    );

    unsafe {
        check("krun_set_log_level", krun_set_log_level(args.log_level))?;
        let ctx = check("krun_create_ctx", krun_create_ctx())? as u32;
        check(
            "krun_set_vm_config",
            krun_set_vm_config(ctx, args.vcpus, args.mem_mib),
        )?;
        check("krun_set_root", krun_set_root(ctx, rootfs_c.as_ptr()))?;
        check("krun_set_workdir", krun_set_workdir(ctx, workdir_c.as_ptr()))?;
        check(
            "krun_set_exec",
            krun_set_exec(
                ctx,
                cmd_c.as_ptr(),
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            ),
        )?;

        eprintln!("→ entering microVM");
        check("krun_start_enter", krun_start_enter(ctx))?;
    }

    Ok(())
}
