//! `.fig` files through the file commands. The importer's own tests (fixtures built in code) are
//! in `figcraft_format::fig`; the local check here runs only when `FIGCRAFT_FIG_SAMPLES` names a
//! folder of `.fig` files with their frames' PNG exports next to them.

use figcraft_doc::{ExportFormat, NodeKind};
use serde_json::json;

use crate::Session;

#[test]
fn damaged_fig_files_fail_cleanly() {
    let dir = std::env::temp_dir().join(format!("figcraft-fig-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("broken.fig");
    std::fs::write(&path, b"fig-kiwi\x65\0\0\0\xff\xff\xff\x7fjunk").unwrap();
    let mut s = Session::new();
    let r = s.execute("file.open", &json!({"path": path.to_string_lossy()}));
    assert!(r.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Local only (`FIGCRAFT_FIG_SAMPLES`): each file opens without warnings, instances link to their
/// mains, and each top frame's 1× PNG export is close to the export saved next to the file.
#[test]
fn local_samples_match_their_exports() {
    let Some(dir) = std::env::var_os("FIGCRAFT_FIG_SAMPLES") else { return };
    let Ok(dir) = std::fs::read_dir(dir) else { return };
    for p in dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "fig")) {
        let mut s = Session::new();
        let r = s.execute("file.open", &json!({"path": p.to_string_lossy()})).unwrap();
        assert_eq!(r["warnings"], json!([]), "{}", p.display());
        let doc = s.doc().unwrap();
        for n in doc.nodes().filter(|n| matches!(n.kind, NodeKind::Instance { .. })) {
            assert!(n.children.iter().all(|c| doc.node(*c).is_some_and(|c| c.source.is_some())), "{}: {}", p.display(), n.name);
        }
        let page = doc.first_page().unwrap();
        for f in doc.top_frames(page) {
            let name = &doc.node(f).unwrap().name;
            let Ok(want) = image::open(p.with_file_name(format!("{name}.png"))) else { continue };
            let got = crate::cmd::file::export_bytes(doc, &[f], ExportFormat::Png, 1.0).unwrap();
            let (got, want) = (image::load_from_memory(&got).unwrap().to_rgb8(), want.to_rgb8());
            // Exports with effects may be cropped differently; compare the ones of the same size.
            if got.dimensions() != want.dimensions() {
                continue;
            }
            let diff: u64 = got.as_raw().iter().zip(want.as_raw()).map(|(a, b)| u64::from(a.abs_diff(*b))).sum();
            let mean = diff as f64 / got.as_raw().len() as f64;
            // Text may use another font where the sample's isn't installed.
            assert!(mean < 4.0, "{name}: mean difference {mean:.2}");
        }
    }
}
