//! `vision_analyze` — the declared core tool name, made functional
//! (feature 016, US6/T049; FR-018 surface completion).
//!
//! Takes an image (file path or data URL) plus a prompt and returns the
//! image as multimodal content so the provider's vision model analyzes it
//! natively. The turn is served by the resolved image model (FR-016 routing
//! in joey-agent-core's resolve_main_turn_model).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{Tool, ToolResult};

/// MIME sniffing (lean: no `infer`/`mime_guess` dependency). Extension first;
/// unknown/missing extensions fall back to magic-byte sniffing so a .bmp or
/// binary blob is never mislabeled as image/jpeg on its way to the provider.
fn mime_for(path: &str) -> Result<&'static str, String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        return Ok("image/png");
    } else if lower.ends_with(".gif") {
        return Ok("image/gif");
    } else if lower.ends_with(".webp") {
        return Ok("image/webp");
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        return Ok("image/jpeg");
    } else if lower.ends_with(".bmp") {
        return Ok("image/bmp");
    } else if lower.ends_with(".tiff") || lower.ends_with(".tif") {
        return Ok("image/tiff");
    } else if lower.ends_with(".avif") {
        return Ok("image/avif");
    } else if lower.ends_with(".heic") {
        return Ok("image/heic");
    }
    // Unknown extension: sniff magic bytes.
    match sniff_magic(path)? {
        Some(mime) => Ok(mime),
        None => Err(format!(
            "cannot determine image type of '{path}': unknown extension and unrecognized magic bytes"
        )),
    }
}

/// Magic-byte sniffing for common image formats.
fn sniff_magic(path: &str) -> Result<Option<&'static str>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("cannot read image '{path}': {e}"))?;
    let mut buf = [0u8; 12];
    let n = f
        .read(&mut buf)
        .map_err(|e| format!("cannot read image '{path}': {e}"))?;
    let b = &buf[..n];
    Ok(if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"BM") {
        Some("image/bmp")
    } else if b.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || b.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        Some("image/tiff")
    } else {
        None
    })
}

/// Read the image and return a data-URL content part for the model.
fn image_part(path: &str) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read image '{path}': {e}"))?;
    if bytes.len() > 15 * 1024 * 1024 {
        return Err("image exceeds 15 MB limit".into());
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mime = mime_for(path)?;
    Ok(json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{};base64,{}", mime, b64) }
    }))
}

/// Display-side mirror of agent-core's image-model routing (FR-016
/// reporting): resolve which model served/serves visual content using the
/// same config-key order (per-provider → global → primary-if-vision) and
/// return the `served_by` object for tool results. The authoritative
/// routing decision happens in joey-agent-core::image_model; this mirror
/// exists because joey-tools cannot depend on that crate (DAG).
pub fn served_by_report() -> Value {
    let cfg = joey_core::config::Config::load()
        .unwrap_or_else(|_| joey_core::config::Config::defaults());
    let provider = cfg.get_str("model.provider", "");
    let primary = cfg.get_str("model.default", "");
    let per_provider = cfg.get_str(&format!("providers.{provider}.image_model"), "");
    let global = cfg.get_str("model.image_model", "");
    let (model, source) = if !per_provider.is_empty() {
        (per_provider, "explicit_per_provider")
    } else if !global.is_empty() {
        (global, "explicit_global")
    } else {
        (primary.clone(), "primary_if_vision")
    };
    json!({ "model": model, "source": source })
}

/// The `vision_analyze` tool.
pub struct VisionAnalyze;

#[async_trait]
impl Tool for VisionAnalyze {
    fn name(&self) -> &str {
        "vision_analyze"
    }

    fn toolset(&self) -> &str {
        "web"
    }

    fn emoji(&self) -> &str {
        "👁️"
    }

    fn description(&self) -> &str {
        "Analyze an image with vision: pass an image file path (png/jpg/gif/webp) \
         and a question. The image is shown to the model's vision capability \
         natively. Use for screenshots, diagrams, photos — anything \
         read_file cannot display. Images up to 15 MB."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "image_path": {
                    "type": "string",
                    "description": "Path to the image file to analyze."
                },
                "question": {
                    "type": "string",
                    "description": "What to look for / answer about the image."
                }
            },
            "required": ["image_path", "question"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let path = match args.get("image_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::Error("missing image_path".into()),
        };
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image.")
            .to_string();
        // Data-URL passthrough: if the caller already has a data URL, use it.
        let part = if path.starts_with("data:image/") {
            json!({
                "type": "image_url",
                "image_url": { "url": path }
            })
        } else {
            match image_part(path) {
                Ok(p) => p,
                Err(e) => return ToolResult::Error(e),
            }
        };
        let served = served_by_report();
        ToolResult::Multimodal(vec![
            json!({
                "type": "text",
                "text": format!(
                    "Analyze this image: {question}\n(served_by: {} via {})",
                    served["model"].as_str().unwrap_or("?"),
                    served["source"].as_str().unwrap_or("?")
                )
            }),
            part,
        ])
    }
}

/// Register the vision tools (currently `vision_analyze`).
pub fn register_vision_tools(registry: &mut crate::registry::ToolRegistry) {
    registry.register(Arc::new(VisionAnalyze));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_sniffing() {
        assert_eq!(mime_for("/tmp/shot.PNG").unwrap(), "image/png");
        assert_eq!(mime_for("a.jpg").unwrap(), "image/jpeg");
        assert_eq!(mime_for("a.jpeg").unwrap(), "image/jpeg");
        assert_eq!(mime_for("a.webp").unwrap(), "image/webp");
        assert_eq!(mime_for("a.gif").unwrap(), "image/gif");
        assert_eq!(mime_for("a.bmp").unwrap(), "image/bmp");
        assert_eq!(mime_for("a.tiff").unwrap(), "image/tiff");
        assert_eq!(mime_for("a.tif").unwrap(), "image/tiff");
        assert_eq!(mime_for("a.avif").unwrap(), "image/avif");
        assert_eq!(mime_for("a.heic").unwrap(), "image/heic");
        // Unknown extension with no file: errors instead of mislabeling.
        assert!(mime_for("noext").is_err());
    }

    #[test]
    fn magic_byte_sniffing_fills_unknown_extensions() {
        let dir = tempfile::tempdir().unwrap();
        // PNG magic under an extensionless filename.
        let p = dir.path().join("screenshot");
        std::fs::write(&p, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        assert_eq!(mime_for(p.to_str().unwrap()).unwrap(), "image/png");
        // JPEG magic.
        let j = dir.path().join("photo");
        std::fs::write(&j, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        assert_eq!(mime_for(j.to_str().unwrap()).unwrap(), "image/jpeg");
        // BMP magic.
        let b = dir.path().join("bitmap");
        std::fs::write(&b, b"BM\x00\x00").unwrap();
        assert_eq!(mime_for(b.to_str().unwrap()).unwrap(), "image/bmp");
        // Unrecognized bytes: error, not a jpeg label.
        let u = dir.path().join("mystery");
        std::fs::write(&u, b"not an image at all").unwrap();
        let err = mime_for(u.to_str().unwrap()).unwrap_err();
        assert!(err.contains("cannot determine image type"), "{err}");
    }

    #[tokio::test]
    async fn missing_file_errors_cleanly() {
        let r = VisionAnalyze
            .execute(
                json!({ "image_path": "/nonexistent/x.png", "question": "what?" }),
                &ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t"),
            )
            .await;
        assert!(r.is_error());
        assert!(r.to_content_string().contains("cannot read image"));
    }

    #[tokio::test]
    async fn tiny_png_produces_multimodal() {
        // 1x1 transparent PNG.
        let png: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a,
        ];
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        std::fs::write(&p, png).unwrap();
        let r = VisionAnalyze
            .execute(
                json!({ "image_path": p.to_str().unwrap(), "question": "describe" }),
                &ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t"),
            )
            .await;
        assert!(!r.is_error());
        let s = r.to_content_string();
        assert!(s.contains("Analyze this image"), "text part present: {s}");
    }

    #[tokio::test]
    async fn data_url_passthrough() {
        let r = VisionAnalyze
            .execute(
                json!({ "image_path": "data:image/png;base64,aGk=", "question": "q" }),
                &ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t"),
            )
            .await;
        assert!(!r.is_error());
    }
}
