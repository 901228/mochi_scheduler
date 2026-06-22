use std::process::Command;

/// Which GPU stack the host exposes, used to pick the right device-isolation
/// environment variable when launching jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    /// No GPUs detected (or detection failed). GPU jobs cannot run.
    None,
}

impl Vendor {
    /// Environment variable(s) that restrict a child process to a subset of
    /// GPUs. We set every name a vendor honours so the isolation sticks
    /// regardless of which runtime (CUDA / HIP / ROCr) the job uses.
    pub fn visible_devices_env(self) -> &'static [&'static str] {
        match self {
            Vendor::Nvidia => &["CUDA_VISIBLE_DEVICES"],
            Vendor::Amd => &["HIP_VISIBLE_DEVICES", "ROCR_VISIBLE_DEVICES"],
            Vendor::None => &[],
        }
    }

    fn parse(s: &str) -> Option<Vendor> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nvidia" | "cuda" => Some(Vendor::Nvidia),
            "amd" | "rocm" | "hip" => Some(Vendor::Amd),
            "none" => Some(Vendor::None),
            _ => None,
        }
    }
}

/// Total number of usable GPUs and the stack they belong to.
#[derive(Debug, Clone, Copy)]
pub struct GpuInfo {
    pub count: u32,
    pub vendor: Vendor,
}

/// Detect the GPUs available on this host.
///
/// `MOCHI_GPU_COUNT` (optionally with `MOCHI_GPU_VENDOR`, default `nvidia`)
/// short-circuits detection entirely; it's the supported way to run without a
/// real GPU (tests, CI, separate queues). Otherwise we probe `nvidia-smi`, then
/// `rocm-smi`, falling back to zero GPUs.
pub fn detect() -> GpuInfo {
    if let Some(count) = std::env::var("MOCHI_GPU_COUNT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
    {
        let vendor = std::env::var("MOCHI_GPU_VENDOR")
            .ok()
            .and_then(|v| Vendor::parse(&v))
            .unwrap_or(Vendor::Nvidia);
        // A zero override means "no GPUs" regardless of the named vendor.
        let vendor = if count == 0 { Vendor::None } else { vendor };
        return GpuInfo { count, vendor };
    }

    if let Some(count) = detect_nvidia() {
        return GpuInfo {
            count,
            vendor: Vendor::Nvidia,
        };
    }
    if let Some(count) = detect_amd() {
        return GpuInfo {
            count,
            vendor: Vendor::Amd,
        };
    }

    GpuInfo {
        count: 0,
        vendor: Vendor::None,
    }
}

/// Build a `Command` for a detection tool. On Windows we set `CREATE_NO_WINDOW`
/// so probing GPUs from the console-less daemon doesn't flash a console window.
fn probe(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Count NVIDIA GPUs via `nvidia-smi -L`, which prints one `GPU N: ...` line per device.
fn detect_nvidia() -> Option<u32> {
    let out = probe("nvidia-smi").arg("-L").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let n = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.trim_start().starts_with("GPU"))
        .count() as u32;
    (n > 0).then_some(n)
}

/// Count AMD GPUs via `rocm-smi --showid`, which prints one `GPU[N]` line per device.
fn detect_amd() -> Option<u32> {
    let out = probe("rocm-smi").arg("--showid").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let n = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.trim_start().starts_with("GPU["))
        .count() as u32;
    (n > 0).then_some(n)
}
