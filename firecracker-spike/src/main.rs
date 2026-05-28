// Minimal Firecracker API client.
//
// Talks plain HTTP/1.1 over the Firecracker UNIX API socket to configure and
// boot a microVM. Intentionally uses raw tokio::UnixStream rather than hyper
// to keep the dep tree tiny — the API is five short requests.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

struct Args {
    socket: PathBuf,
    kernel: PathBuf,
    rootfs: PathBuf,
    vcpus: u32,
    mem_mib: u32,
    boot_args: String,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut socket = None;
        let mut kernel = None;
        let mut rootfs = None;
        let mut vcpus: u32 = 1;
        let mut mem_mib: u32 = 128;
        let mut boot_args =
            String::from("console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on");

        let mut iter = env::args().skip(1);
        while let Some(a) = iter.next() {
            match a.as_str() {
                "--socket" => socket = iter.next().map(PathBuf::from),
                "--kernel" => kernel = iter.next().map(PathBuf::from),
                "--rootfs" => rootfs = iter.next().map(PathBuf::from),
                "--vcpus" => {
                    vcpus = iter
                        .next()
                        .ok_or("missing value for --vcpus")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
                "--mem-mib" => {
                    mem_mib = iter
                        .next()
                        .ok_or("missing value for --mem-mib")?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
                "--boot-args" => {
                    boot_args = iter.next().ok_or("missing value for --boot-args")?;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown arg: {other}")),
            }
        }

        Ok(Self {
            socket: socket.ok_or("--socket required")?,
            kernel: kernel.ok_or("--kernel required")?,
            rootfs: rootfs.ok_or("--rootfs required")?,
            vcpus,
            mem_mib,
            boot_args,
        })
    }
}

fn print_help() {
    eprintln!(
        "firecracker-spike \\
  --socket  PATH     path to firecracker --api-sock
  --kernel  PATH     vmlinux image
  --rootfs  PATH     ext4 root filesystem
  [--vcpus  N        default 1]
  [--mem-mib N       default 128]
  [--boot-args STR   default: console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on]"
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n");
            print_help();
            return ExitCode::from(2);
        }
    };

    if let Err(e) = drive_microvm(args).await {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn drive_microvm(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("→ socket  {}", args.socket.display());
    println!("→ kernel  {}", args.kernel.display());
    println!("→ rootfs  {}", args.rootfs.display());
    println!("→ vcpus   {}", args.vcpus);
    println!("→ memMiB  {}", args.mem_mib);

    request(
        &args.socket,
        "PUT",
        "/boot-source",
        json!({
            "kernel_image_path": args.kernel.to_string_lossy(),
            "boot_args": args.boot_args,
        }),
    )
    .await?;

    request(
        &args.socket,
        "PUT",
        "/drives/rootfs",
        json!({
            "drive_id": "rootfs",
            "path_on_host": args.rootfs.to_string_lossy(),
            "is_root_device": true,
            "is_read_only": false,
        }),
    )
    .await?;

    request(
        &args.socket,
        "PUT",
        "/machine-config",
        json!({
            "vcpu_count": args.vcpus,
            "mem_size_mib": args.mem_mib,
            "smt": false,
        }),
    )
    .await?;

    request(
        &args.socket,
        "PUT",
        "/actions",
        json!({ "action_type": "InstanceStart" }),
    )
    .await?;

    println!("✓ microVM started — serial output will land in firecracker stdout");
    Ok(())
}

async fn request(
    socket: &Path,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let body_str = body.to_string();
    let req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Accept: application/json\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body_str}",
        len = body_str.len()
    );

    let mut stream = UnixStream::connect(socket).await?;
    stream.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;

    let resp = String::from_utf8_lossy(&buf);
    let status_line = resp.lines().next().unwrap_or("(no response)");
    println!("  {method:<4} {path:<18} → {status_line}");

    // Firecracker returns 2xx on success (typically 204 No Content).
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .map(|c| (200..300).contains(&c))
        .unwrap_or(false);

    if !ok {
        println!("---\n{resp}\n---");
        return Err(format!("firecracker rejected {method} {path}").into());
    }
    Ok(())
}
