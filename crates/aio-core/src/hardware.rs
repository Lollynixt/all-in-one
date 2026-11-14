//! Hardware profile, model fit estimate and "Recommended" ranking.
//! Recommendation only ranks; it never restricts what the user can pick.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareProfile {
    pub cpu_name: String,
    pub cpu_threads: usize,
    pub ram_bytes: u64,
    /// Dedicated GPU memory if known. Detection of this is vendor specific and arrives in a later phase;
    /// until then users can override it in the Models tab.
    pub vram_bytes: Option<u64>,
}

const GB: u64 = 1024 * 1024 * 1024;

impl HardwareProfile {
    pub fn detect() -> Self {
        use sysinfo::System;
        let Ok(mut sys) = System::new() else {
            // Probing failed: report a conservative profile rather than crash.
            return Self { cpu_name: "Unknown CPU".into(), cpu_threads: 1, ram_bytes: 8 * GB, vram_bytes: nvidia_vram() };
        };
        sys.refresh_memory();
        sys.refresh_cpu_all();
        let ram_bytes = sys.total_memory();
        let vram_bytes = nvidia_vram().or_else(|| {
            // Apple Silicon shares one memory pool between CPU and GPU.
            cfg!(all(target_os = "macos", target_arch = "aarch64")).then(|| apple_unified_gpu_budget(ram_bytes))
        });
        let cpu_name = sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_else(|| "Unknown CPU".into());
        Self { cpu_name, cpu_threads: sys.cpus().len().max(1), ram_bytes, vram_bytes }
    }
    pub fn with_vram_gb(mut self, gb: f64) -> Self {
        self.vram_bytes = Some((gb * GB as f64) as u64);
        self
    }
    pub fn ram_gb(&self) -> f64 {
        self.ram_bytes as f64 / GB as f64
    }
    pub fn vram_gb(&self) -> Option<f64> {
        self.vram_bytes.map(|v| v as f64 / GB as f64)
    }
}

/// macOS lets the GPU use roughly two thirds of unified memory by default.
pub fn apple_unified_gpu_budget(ram_bytes: u64) -> u64 {
    (ram_bytes as f64 * 0.66) as u64
}

/// Best effort NVIDIA detection through `nvidia-smi`. Returns None when absent.
fn nvidia_vram() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mib: u64 = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<u64>().ok()).max()?;
    Some(mib * 1024 * 1024)
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogModel {
    pub family: &'static str,
    pub params_b: f32,
    pub quant: &'static str,
    /// Weights size on disk, in GB.
    pub size_gb: f32,
    /// Name to pass to Ollama.
    pub ollama_tag: &'static str,
    pub tags: &'static [&'static str],
}

/// Starter catalogue. The real one will be a versioned, updatable file.
pub fn catalogue() -> Vec<CatalogModel> {
    vec![
        CatalogModel { family: "Llama 3.2", params_b: 3.0, quant: "Q4_K_M", size_gb: 2.0, ollama_tag: "llama3.2:3b", tags: &["chat", "small"] },
        CatalogModel { family: "Llama 3.1", params_b: 8.0, quant: "Q4_K_M", size_gb: 4.9, ollama_tag: "llama3.1:8b", tags: &["chat"] },
        CatalogModel { family: "Qwen2.5 Coder", params_b: 7.0, quant: "Q4_K_M", size_gb: 4.7, ollama_tag: "qwen2.5-coder:7b", tags: &["code"] },
        CatalogModel { family: "Qwen2.5", params_b: 14.0, quant: "Q4_K_M", size_gb: 9.0, ollama_tag: "qwen2.5:14b", tags: &["chat"] },
        CatalogModel { family: "Qwen2.5", params_b: 32.0, quant: "Q4_K_M", size_gb: 20.0, ollama_tag: "qwen2.5:32b", tags: &["chat", "large"] },
        CatalogModel { family: "Llama 3.3", params_b: 70.0, quant: "Q4_K_M", size_gb: 43.0, ollama_tag: "llama3.3:70b", tags: &["chat", "large"] },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Fit {
    /// Does not fit even in system RAM.
    TooLarge,
    /// CPU only, or heavy offload. Works but slow.
    Slow,
    /// Weights split between GPU and RAM.
    Partial,
    /// Fully in GPU memory with headroom for context.
    Comfortable,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Comfortable => "Fits in GPU memory",
            Fit::Partial => "Partly offloaded to RAM",
            Fit::Slow => "CPU only, slow",
            Fit::TooLarge => "Too large for this machine",
        }
    }
}

/// Memory needed beyond weights for KV cache and runtime, at a modest context.
fn overhead_gb(model: &CatalogModel) -> f32 {
    1.0 + model.params_b * 0.03
}

pub fn estimate_fit(model: &CatalogModel, hw: &HardwareProfile) -> Fit {
    let need = model.size_gb + overhead_gb(model);
    let ram_usable = hw.ram_gb() as f32 * 0.75;
    match hw.vram_gb() {
        Some(v) if need <= v as f32 * 0.92 => Fit::Comfortable,
        Some(v) if v > 2.0 && need <= (v as f32 * 0.92 + ram_usable) => Fit::Partial,
        _ if need <= ram_usable => Fit::Slow,
        _ => Fit::TooLarge,
    }
}

#[derive(Debug, Clone)]
pub struct Ranked {
    pub model: CatalogModel,
    pub fit: Fit,
    pub recommended: bool,
}

/// Ranks the whole catalogue: best fit first, then largest model that still fits well.
/// The single top entry whose fit is Comfortable (or Partial if none) is marked recommended.
pub fn rank(hw: &HardwareProfile, prefer_tag: Option<&str>) -> Vec<Ranked> {
    let mut v: Vec<Ranked> = catalogue()
        .into_iter()
        .filter(|m| prefer_tag.is_none_or(|t| m.tags.contains(&t)))
        .map(|m| {
            let fit = estimate_fit(&m, hw);
            Ranked { model: m, fit, recommended: false }
        })
        .collect();
    v.sort_by(|a, b| {
        let usable = |f: Fit| matches!(f, Fit::Comfortable | Fit::Partial) as u8;
        usable(b.fit)
            .cmp(&usable(a.fit))
            .then(b.fit.cmp(&a.fit))
            .then(b.model.params_b.partial_cmp(&a.model.params_b).unwrap_or(std::cmp::Ordering::Equal))
    });
    if let Some(first) = v.first_mut() {
        if first.fit != Fit::TooLarge {
            first.recommended = true;
        }
    }
    v
}

pub fn recommended(hw: &HardwareProfile, prefer_tag: Option<&str>) -> Option<CatalogModel> {
    rank(hw, prefer_tag).into_iter().find(|r| r.recommended).map(|r| r.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(ram: u64, vram: Option<u64>) -> HardwareProfile {
        HardwareProfile { cpu_name: "t".into(), cpu_threads: 8, ram_bytes: ram * GB, vram_bytes: vram.map(|v| v * GB) }
    }

    #[test]
    fn twelve_gb_gpu_picks_mid_size() {
        let r = recommended(&hw(32, Some(12)), Some("chat")).unwrap();
        assert_eq!(r.ollama_tag, "qwen2.5:14b"); // 9 GB + overhead fits 12 GB
    }

    #[test]
    fn apple_budget_is_two_thirds() {
        assert_eq!(apple_unified_gpu_budget(16 * GB) / (GB / 100), 1056);
    }

    #[test]
    fn small_machine_gets_small_model() {
        let r = recommended(&hw(8, None), Some("chat")).unwrap();
        assert_eq!(r.ollama_tag, "llama3.2:3b");
    }

    #[test]
    fn nothing_fits_tiny_machine() {
        assert!(recommended(&hw(2, None), None).is_none());
    }

    #[test]
    fn everything_stays_selectable() {
        // Ranking never removes models, only orders them.
        assert_eq!(rank(&hw(8, None), None).len(), catalogue().len());
    }

    #[test]
    fn partial_offload_when_vram_short() {
        let m = catalogue().into_iter().find(|m| m.ollama_tag == "qwen2.5:32b").unwrap();
        assert_eq!(estimate_fit(&m, &hw(32, Some(12))), Fit::Partial);
    }
}
