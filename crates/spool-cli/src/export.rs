//! `export`: a page, a frame or a layer written as png, jpg, webp, SVG or PDF.
//!
//! Raster formats go through `figcraft_render`, vector through `figcraft_svg` and print through
//! `figcraft_pdf`; the exported area and the depth limit come from the renderer, which is what the
//! engine's own `file.export` command uses, so a CLI export and an in-app export agree.

use figcraft_doc::{Document, ExportFormat, ExportSetting, NodeId};
use serde_json::{json, Value};

use crate::CliError;
use crate::CliResult;

/// Largest and smallest scale an export accepts (`figcraft_render` clamps to 64 itself).
const MAX_SCALE: f64 = 64.0;
const MIN_SCALE: f64 = 0.01;

/// One exported file.
pub struct Exported {
    pub bytes: Vec<u8>,
    /// Pixel size, for raster formats.
    pub size: Option<(u32, u32)>,
}

impl Exported {
    /// What `export` reports on stdout.
    pub fn report(&self, path: &str, format: ExportFormat, scale: f64) -> Value {
        let mut v = json!({
            "path": path,
            "format": format.ext(),
            "scale": scale,
            "bytes": self.bytes.len(),
        });
        if let (Value::Object(o), Some((w, h))) = (&mut v, self.size) {
            o.insert("width".into(), json!(w));
            o.insert("height".into(), json!(h));
        }
        v
    }

    /// `wrote <path> (PNG 1200×800, 54321 bytes)`.
    pub fn describe(&self, path: &str, format: ExportFormat) -> String {
        let dims = self
            .size
            .map(|(w, h)| format!(" {w}×{h}"))
            .unwrap_or_default();
        format!(
            "wrote {path} ({}{}, {} bytes)",
            format.label(),
            dims,
            self.bytes.len()
        )
    }
}

/// The export format `--format` or the output's extension names.
pub fn format(name: &str) -> CliResult<ExportFormat> {
    ExportFormat::from_ext(name).ok_or_else(|| {
        CliError::Usage(format!(
            "unknown export format `{name}` (png|jpg|svg|pdf|webp; `.figcraft` and `.json` are written by `save`)"
        ))
    })
}

/// Parse `--scale`: a positive, finite number, clamped to the range the renderer accepts.
pub fn scale(text: Option<&str>) -> CliResult<f64> {
    let Some(t) = text else { return Ok(1.0) };
    let v: f64 = t
        .parse()
        .map_err(|_| CliError::Usage(format!("--scale needs a number, got `{t}`")))?;
    if !v.is_finite() || v <= 0.0 {
        return Err(CliError::Usage(format!(
            "--scale needs a positive number, got `{t}`"
        )));
    }
    Ok(v.clamp(MIN_SCALE, MAX_SCALE))
}

/// Encode `ids` (bottom to top) in `format` at `scale`.
pub fn encode(
    doc: &Document,
    ids: &[NodeId],
    format: ExportFormat,
    scale: f64,
) -> CliResult<Exported> {
    if ids.is_empty() {
        return Err(CliError::Failed(
            "nothing to export: the page has no layers".into(),
        ));
    }
    let setting = ExportSetting {
        format,
        ..ExportSetting::default()
    };
    if matches!(format, ExportFormat::Svg | ExportFormat::Pdf) {
        figcraft_render::check_export_depth(doc, ids)
            .map_err(|e| CliError::Failed(e.to_string()))?;
    }
    match format {
        ExportFormat::Svg => {
            let opts = figcraft_svg::SvgOptions {
                bounds: None,
                ..figcraft_svg::SvgOptions::default()
            };
            let text = figcraft_svg::try_export_svg(&doc.in_srgb(), ids, &opts)
                .map_err(|e| CliError::Failed(format!("can't export SVG: {e}")))?;
            Ok(Exported {
                bytes: text.into_bytes(),
                size: None,
            })
        }
        ExportFormat::Pdf => {
            // Top-level frames export one page each; anything else shares a page.
            let one_per_frame = ids
                .iter()
                .all(|i| doc.node(*i).is_some_and(|n| n.kind.is_frame_like()));
            let groups: Vec<Vec<NodeId>> = if one_per_frame {
                ids.iter().map(|i| vec![*i]).collect()
            } else {
                vec![ids.to_vec()]
            };
            let opts = figcraft_pdf::PdfOptions {
                title: Some(doc.name.clone()),
                ..figcraft_pdf::PdfOptions::default()
            };
            let bytes = figcraft_pdf::export_groups(&doc.in_srgb(), &groups, &opts)
                .map_err(|e| CliError::Failed(format!("can't export PDF: {e}")))?;
            Ok(Exported { bytes, size: None })
        }
        _ => {
            let area =
                figcraft_engine::cmd::file::export_area(doc, ids, &setting).ok_or_else(|| {
                    CliError::Failed(
                        "nothing to export: the layers have no area (are they hidden or empty?)"
                            .into(),
                    )
                })?;
            let pix = figcraft_render::render_area(doc, ids, area, scale).ok_or_else(|| {
                CliError::Failed(
                    "can't render: the area is empty or larger than 16384 px a side".into(),
                )
            })?;
            let size = (pix.width(), pix.height());
            let bytes = figcraft_render::encode_export(&pix, format, doc.color_profile)
                .map_err(|e| CliError::Failed(format!("can't encode {}: {e}", format.label())))?;
            Ok(Exported {
                bytes,
                size: Some(size),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn formats_come_from_the_engine_enum() {
        assert_eq!(format("png").unwrap(), ExportFormat::Png);
        assert_eq!(format("JPEG").unwrap(), ExportFormat::Jpg);
        assert_eq!(format(".pdf").unwrap(), ExportFormat::Pdf);
        assert_eq!(format("webp").unwrap(), ExportFormat::Webp);
        assert_eq!(format("svg").unwrap(), ExportFormat::Svg);
        for bad in ["figcraft", "json", "fig", "", "tif"] {
            let e = format(bad).unwrap_err();
            assert!(matches!(e, CliError::Usage(_)), "{bad} is a usage error");
        }
    }

    #[test]
    fn scale_defaults_to_one_and_rejects_nonsense() {
        assert_eq!(scale(None).unwrap(), 1.0);
        assert_eq!(scale(Some("2")).unwrap(), 2.0);
        assert_eq!(scale(Some("0.5")).unwrap(), 0.5);
        assert_eq!(scale(Some("1000")).unwrap(), MAX_SCALE);
        for bad in ["0", "-1", "two", "NaN", "inf"] {
            assert!(scale(Some(bad)).is_err(), "--scale {bad} should be refused");
        }
    }

    #[test]
    fn an_empty_id_list_is_a_failure_not_a_panic() {
        let doc = Document::new("empty");
        assert!(encode(&doc, &[], ExportFormat::Png, 1.0).is_err());
    }
}
