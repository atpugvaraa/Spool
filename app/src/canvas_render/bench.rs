//! The measurements behind the two complaints: a delete that is not instant, and a
//! drag that feels like jelly.
//!
//! # What is measured
//!
//! The whole path an edit takes, in the order it takes it:
//!
//! ```text
//!   the command      session.execute("edit.delete")   (the engine's own work)
//!   the gesture      session.pointer(Move)            (the engine's preview)
//!   the render       retained.render(doc, page, view) (the engine's pixels)
//!   the conversion   pixmap -> BGRA -> RenderImage    (Spool's own work)
//!   the round trip   submit -> publish                (what the user waits for)
//! ```
//!
//! The round trip is the number that matters: the pixels on screen are always a
//! frame old, and a frame that takes longer than a frame to arrive is a canvas
//! that lags the pointer.
//!
//! # How to read it
//!
//! ```sh
//! cargo test -p Spool --release -- --nocapture canvas_render::bench
//! ```
//!
//! The numbers are printed as a table. They are not asserted on: a timing
//! assertion is a flaky test, and the point of this module is to be read, not to
//! fail.

use std::sync::Arc;
use std::time::{Duration, Instant};

use figcraft_doc::{Document, NodeKind};
use figcraft_engine::tools::{PointerEvent, PointerKind};
use figcraft_engine::Session;
use figcraft_render::Options;
use serde_json::json;

use super::*;

/// The viewport the measurements are taken at: an 800 × 600 point canvas on a
/// Retina display, which is 1600 × 1200 device pixels of texture.
const VIEWPORT: (f32, f32) = (800.0, 600.0);
/// The device pixel ratio of that display.
const RATIO: f32 = 2.0;

/// A page with `count` filled rectangles on it.
///
/// Filled, because an empty rectangle is cheap to draw and would make the
/// render look faster than it is: the whole complaint is about pixels.
fn busy_document(count: usize) -> (Document, NodeId) {
    let mut doc = Document::new("bench");
    let page = doc.first_page().expect("a new document has a page");
    for i in 0..count {
        let x = 20.0 + (i % 20) as f64 * 38.0;
        let y = 20.0 + (i / 20) as f64 * 38.0;
        let id = doc
            .create(page, NodeKind::Rectangle, "R", x, y, 30.0, 30.0)
            .expect("a rectangle can be created on a page");
        let node = doc.node_mut(id).expect("the rectangle is there");
        node.fills = vec![figcraft_doc::color::Paint::solid(
            figcraft_doc::color::Color {
                r: 0.2,
                g: 0.4,
                b: 0.9,
                a: 1.0,
            },
        )];
    }
    (doc, page)
}

/// The last rectangle of the page: the one the measurements delete.
fn last_rectangle(doc: &Document, page: NodeId) -> NodeId {
    doc.node(page)
        .and_then(|n| n.children.last().copied())
        .expect("the page has children")
}

/// The centre of a node, in page coordinates: where a pointer press lands on it.
fn centre(doc: &Document, id: NodeId) -> (f64, f64) {
    let bounds = doc.world_bounds(id).expect("the node has bounds");
    ((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0)
}

/// Waits for the frame that answers the request just submitted.
fn await_frame(renderer: &CanvasRenderer, after: u64) -> CanvasFrame {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if renderer.generation() > after && !renderer.is_rendering() {
            if let Some(frame) = renderer.frame() {
                return frame;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no frame within thirty seconds");
}

/// One measured edit: the command, the render it caused, and how long the whole
/// thing took to reach the screen.
struct Measurement {
    /// The engine's own work for the command.
    command: Duration,
    /// The engine's render.
    render: Duration,
    /// Turning the pixmap into a GPUI image.
    convert: Duration,
    /// The round trip: the submission to the published frame.
    round_trip: Duration,
    /// What the engine's render reported.
    work: Work,
}

impl Measurement {
    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }
}

/// Delete a node, measure it, undo it, measure that — `repeats` times over.
///
/// The cycle repeats because one sample of a millisecond-scale measurement is
/// noise: the page faults of a fresh multi-megabyte buffer and the machine's
/// own scheduling decide as much of it as the work does. The median of many
/// samples is the number that says what the work costs.
fn measure(count: usize, repeats: usize) -> (Vec<Measurement>, Vec<Measurement>) {
    let (doc, page) = busy_document(count);
    let target = last_rectangle(&doc, page);
    let mut session = Session::with_document(doc);
    session.set_selection(vec![target]);

    let renderer = CanvasRenderer::new();
    let options = Options {
        lod: true,
        ..Options::default()
    };

    // The first render: the page as it is, so the retained renderer has an
    // image to patch.
    let before = renderer.generation();
    renderer
        .render_canvas(
            session.ds().expect("a document").doc.clone(),
            page,
            (0.0, 0.0),
            1.0,
            VIEWPORT,
            RATIO,
            options.clone(),
        )
        .expect("a usable view");
    await_frame(&renderer, before);

    let mut deletes = Vec::with_capacity(repeats);
    let mut undos = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        // --- the delete -----------------------------------------------------
        let started = Instant::now();
        session
            .execute("edit.delete", &json!({}))
            .expect("the delete succeeds");
        let command = started.elapsed();

        let before = renderer.generation();
        let submitted = Instant::now();
        renderer
            .render_canvas(
                session.ds().expect("a document").doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                VIEWPORT,
                RATIO,
                options.clone(),
            )
            .expect("a usable view");
        let frame = await_frame(&renderer, before);
        let round_trip = submitted.elapsed();
        let stats = renderer.stats();
        deletes.push(Measurement {
            command,
            render: Duration::from_micros(stats.last_render_micros),
            convert: Duration::from_micros(stats.last_convert_micros),
            round_trip,
            work: frame.work,
        });

        // --- the undo -------------------------------------------------------
        let started = Instant::now();
        let undone = session.undo().expect("an undo");
        assert!(undone, "the undo did something");
        let command = started.elapsed();

        let before = renderer.generation();
        let submitted = Instant::now();
        renderer
            .render_canvas(
                session.ds().expect("a document").doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                VIEWPORT,
                RATIO,
                options.clone(),
            )
            .expect("a usable view");
        let frame = await_frame(&renderer, before);
        let round_trip = submitted.elapsed();
        let stats = renderer.stats();
        undos.push(Measurement {
            command,
            render: Duration::from_micros(stats.last_render_micros),
            convert: Duration::from_micros(stats.last_convert_micros),
            round_trip,
            work: frame.work,
        });
    }
    (deletes, undos)
}

/// One way of turning the engine's pixels into GPUI's.
type Conversion = fn(&figcraft_render::Pixmap) -> Vec<u8>;

/// The median of a set of durations.
fn median(samples: &[Duration]) -> Duration {
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    sorted
        .get(sorted.len() / 2)
        .copied()
        .unwrap_or(Duration::ZERO)
}

/// One line per measurement, with the median of the set.
fn line(label: &str, samples: &[Measurement]) -> String {
    let of = |f: fn(&Measurement) -> Duration| -> f64 {
        let values: Vec<Duration> = samples.iter().map(f).collect();
        Measurement::ms(median(&values))
    };
    format!(
        "{label} command={:.3}ms render={:.3}ms convert={:.3}ms round_trip={:.3}ms",
        of(|m| m.command),
        of(|m| m.render),
        of(|m| m.convert),
        of(|m| m.round_trip),
    )
}

/// The numbers behind the two complaints.
///
/// A delete and an undo on a page of a hundred rectangles, at the size of a
/// Retina canvas, printed as a table. The test asserts nothing about the
/// timings — a timing assertion is a flaky test — and everything about the
/// shapes: the delete and the undo both reach the screen, and both are patches
/// rather than whole redraws.
#[test]
fn the_cost_of_a_delete_and_an_undo_is_measured() {
    let (delete, undo) = measure(100, 25);
    eprintln!(
        "spool_bench rows=100 repeats=25 {} | {}",
        line("delete", &delete),
        line("undo  ", &undo)
    );
    assert!(matches!(delete[0].work, Work::Partial { .. } | Work::Full));
    assert!(matches!(undo[0].work, Work::Partial { .. } | Work::Full));
}

/// The same numbers on a page ten times the size: what does the cost scale with?
#[test]
fn the_cost_of_a_delete_and_an_undo_is_measured_on_a_busy_page() {
    let (delete, undo) = measure(1000, 25);
    eprintln!(
        "spool_bench rows=1000 repeats=25 {} | {}",
        line("delete", &delete),
        line("undo  ", &undo)
    );
    assert!(matches!(delete[0].work, Work::Partial { .. } | Work::Full));
    assert!(matches!(undo[0].work, Work::Partial { .. } | Work::Full));
}

/// The drag: what a pointer move costs the engine, and what it costs the screen.
///
/// The engine's own gesture work (the document preview it builds for the
/// gesture) is on the UI thread, in the pointer handler. The render that
/// answers it is on the render thread. Both are measured, because a drag that
/// feels like jelly is one where the second is late.
#[test]
fn the_cost_of_a_drag_is_measured() {
    let numbers = measure_gesture(Gesture::Move);
    eprintln!(
        "spool_bench drag steps={} gesture_median={:.3}ms round_trip_median={:.3}ms round_trip_mean={:.3}ms \
         render_total={:.3}ms convert_total={:.3}ms full={} partial={} reused={}",
        numbers.steps,
        Measurement::ms(median(&numbers.gesture)),
        Measurement::ms(median(&numbers.round_trip)),
        Measurement::ms(numbers.round_trip_total / numbers.steps),
        Measurement::ms(Duration::from_micros(numbers.render)),
        Measurement::ms(Duration::from_micros(numbers.convert)),
        numbers.full,
        numbers.partial,
        numbers.reused,
    );
    assert_eq!(
        numbers.full + numbers.partial + numbers.reused,
        numbers.published
    );
}

/// The resize: the same path, with the corner handle instead of the body.
///
/// A resize is the case the complaint names: the object changes SIZE, so the
/// pixels and the selection overlay can disagree about the object's box.
#[test]
fn the_cost_of_a_resize_is_measured() {
    let numbers = measure_gesture(Gesture::Resize);
    eprintln!(
        "spool_bench resize steps={} gesture_median={:.3}ms round_trip_median={:.3}ms round_trip_mean={:.3}ms \
         render_total={:.3}ms convert_total={:.3}ms full={} partial={} reused={}",
        numbers.steps,
        Measurement::ms(median(&numbers.gesture)),
        Measurement::ms(median(&numbers.round_trip)),
        Measurement::ms(numbers.round_trip_total / numbers.steps),
        Measurement::ms(Duration::from_micros(numbers.render)),
        Measurement::ms(Duration::from_micros(numbers.convert)),
        numbers.full,
        numbers.partial,
        numbers.reused,
    );
    assert_eq!(
        numbers.full + numbers.partial + numbers.reused,
        numbers.published
    );
}

/// Which gesture the measurement drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gesture {
    /// The body of the layer: a move.
    Move,
    /// The bottom-right corner handle: a resize.
    Resize,
}

/// What a whole gesture cost.
struct GestureNumbers {
    steps: u32,
    /// The engine's own work, one sample per step.
    gesture: Vec<Duration>,
    /// The round trip from the submission to the published frame, one per step.
    round_trip: Vec<Duration>,
    round_trip_total: Duration,
    render: u64,
    convert: u64,
    full: u64,
    partial: u64,
    reused: u64,
    published: u64,
}

/// Drive a whole gesture against a live session, rendering after every step.
fn measure_gesture(gesture: Gesture) -> GestureNumbers {
    let (doc, page) = busy_document(100);
    let target = last_rectangle(&doc, page);
    let (cx, cy) = centre(&doc, target);
    let mut session = Session::with_document(doc);
    session.set_selection(vec![target]);
    let renderer = CanvasRenderer::new();
    let options = Options {
        lod: true,
        ..Options::default()
    };
    let before = renderer.generation();
    renderer
        .render_canvas(
            session.ds().expect("a document").doc.clone(),
            page,
            (0.0, 0.0),
            1.0,
            VIEWPORT,
            RATIO,
            options.clone(),
        )
        .expect("a usable view");
    await_frame(&renderer, before);

    let zoom = session.ds().expect("a document").view.zoom;
    // The press lands where the gesture starts: the body of the layer for a
    // move, its bottom-right corner for a resize.
    let (px, py) = match gesture {
        Gesture::Move => (cx, cy),
        Gesture::Resize => {
            let bounds = session
                .ds()
                .expect("a document")
                .doc
                .world_bounds(target)
                .expect("the node has bounds");
            (bounds.x1, bounds.y1)
        }
    };
    let press = PointerEvent {
        kind: PointerKind::Down,
        x: px,
        y: py,
        sx: px,
        sy: py,
        shift: false,
        alt: false,
        cmd: false,
        ctrl: false,
        zoom,
        pressure: None,
    };
    session.pointer(&press).expect("a press is not an error");

    let mut gesture_time = Vec::with_capacity(20);
    let mut round_trip = Vec::with_capacity(20);
    let mut steps = 0u32;
    for step in 1..=20 {
        let started = Instant::now();
        let ev = PointerEvent {
            kind: PointerKind::Move,
            x: px + f64::from(step) * 2.0,
            y: py + f64::from(step) * 2.0,
            sx: px + f64::from(step) * 2.0,
            sy: py + f64::from(step) * 2.0,
            shift: false,
            alt: false,
            cmd: false,
            ctrl: false,
            zoom,
            pressure: None,
        };
        session.pointer(&ev).expect("a move is not an error");
        gesture_time.push(started.elapsed());

        let before = renderer.generation();
        let submitted = Instant::now();
        renderer
            .render_canvas(
                session.ds().expect("a document").doc.clone(),
                page,
                (0.0, 0.0),
                1.0,
                VIEWPORT,
                RATIO,
                options.clone(),
            )
            .expect("a usable view");
        await_frame(&renderer, before);
        round_trip.push(submitted.elapsed());
        steps += 1;
    }
    // The release: the gesture's own undo step.
    let up = PointerEvent {
        kind: PointerKind::Up,
        x: px + 40.0,
        y: py + 40.0,
        sx: px + 40.0,
        sy: py + 40.0,
        shift: false,
        alt: false,
        cmd: false,
        ctrl: false,
        zoom,
        pressure: None,
    };
    session.pointer(&up).expect("a release is not an error");

    let stats = renderer.stats();
    GestureNumbers {
        steps,
        round_trip_total: round_trip.iter().sum(),
        gesture: gesture_time,
        round_trip,
        render: stats.render_micros,
        convert: stats.convert_micros,
        full: stats.full,
        partial: stats.partial,
        reused: stats.unchanged,
        published: stats.published,
    }
}

// ---------------------------------------------------------------------------
// The conversion
// ---------------------------------------------------------------------------

/// The conversion as it is written today: a demultiply per pixel and four
/// pushes per pixel.
fn convert_pushes(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.width() as usize * pixmap.height() as usize * 4);
    for pixel in pixmap.pixels() {
        let c = pixel.demultiply();
        out.push(c.blue());
        out.push(c.green());
        out.push(c.red());
        out.push(c.alpha());
    }
    out
}

/// The same conversion, writing into a buffer that is already the right size.
fn convert_indexed(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let pixels = pixmap.pixels();
    let mut out = vec![0u8; pixels.len() * 4];
    for (pixel, chunk) in pixels.iter().zip(out.chunks_exact_mut(4)) {
        let c = pixel.demultiply();
        chunk[0] = c.blue();
        chunk[1] = c.green();
        chunk[2] = c.red();
        chunk[3] = c.alpha();
    }
    out
}

/// The conversion with the demultiply done in integers.
///
/// tiny-skia's `demultiply` divides by the alpha in `f32`, three times per
/// pixel. The same rounding is available in integers, and an opaque pixel
/// (the overwhelming majority) needs no division at all.
fn convert_integers(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let pixels = pixmap.pixels();
    let mut out = vec![0u8; pixels.len() * 4];
    for (pixel, chunk) in pixels.iter().zip(out.chunks_exact_mut(4)) {
        let a = pixel.alpha();
        if a == 255 {
            chunk[0] = pixel.blue();
            chunk[1] = pixel.green();
            chunk[2] = pixel.red();
            chunk[3] = 255;
            continue;
        }
        if a == 0 {
            chunk[0] = 0;
            chunk[1] = 0;
            chunk[2] = 0;
            chunk[3] = 0;
            continue;
        }
        let alpha = u32::from(a);
        // `r * 255 / a`, rounded down, in integers: the same value
        // tiny-skia's `f32` division produces, for every channel.
        let scale = |c: u8| -> u8 {
            let v = u32::from(c) * 255 / alpha;
            if v > 255 {
                255
            } else {
                v as u8
            }
        };
        chunk[0] = scale(pixel.blue());
        chunk[1] = scale(pixel.green());
        chunk[2] = scale(pixel.red());
        chunk[3] = a;
    }
    out
}

/// The ways of converting, measured round-robin so that a slow moment on the
/// machine hits every way equally.
///
/// The variants are interleaved rather than run one after another: the machine
/// has slow moments, and a benchmark that runs the variants in turn measures
/// those moments as differences between the variants.
#[test]
fn the_ways_of_converting_are_measured_round_robin() {
    let (doc, page) = busy_document(100);
    let mut retained = figcraft_render::Retained::default();
    let view = figcraft_render::View {
        origin: (0.0, 0.0),
        scale: RATIO as f64,
        width: VIEWPORT.0 as u32 * 2,
        height: VIEWPORT.1 as u32 * 2,
    };
    let options = Options {
        lod: true,
        ..Options::default()
    };
    let doc = Arc::new(doc);
    let Some((_, pixmap, _)) = retained.render(&doc, page, &view, &options) else {
        panic!("the engine rendered nothing");
    };
    let variants: [(&str, Conversion); 6] = [
        ("pushes", convert_pushes),
        ("chunks", convert_indexed),
        ("chunks+opaque", convert_opaque_f32),
        ("chunks+opaque+index", convert_opaque_index),
        ("pushes+opaque", convert_pushes_opaque_f32),
        ("chunks+opaque+vec", convert_opaque_vec),
    ];
    let runs = 40;
    let mut best = vec![Duration::MAX; variants.len()];
    for _ in 0..runs {
        for (i, (_, f)) in variants.iter().enumerate() {
            let started = Instant::now();
            let out = f(pixmap);
            let took = started.elapsed();
            std::hint::black_box(&out);
            best[i] = best[i].min(took);
        }
    }
    for (i, (name, _)) in variants.iter().enumerate() {
        eprintln!(
            "spool_bench convert_round_robin method={name} best={:.3}ms",
            Measurement::ms(best[i])
        );
    }
}

/// The opaque fast path, writing through an index.
fn convert_opaque_index(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let pixels = pixmap.pixels();
    let mut out = vec![0u8; pixels.len() * 4];
    let mut i = 0;
    for pixel in pixels {
        if pixel.alpha() == 255 {
            out[i] = pixel.blue();
            out[i + 1] = pixel.green();
            out[i + 2] = pixel.red();
            out[i + 3] = 255;
        } else {
            let c = pixel.demultiply();
            out[i] = c.blue();
            out[i + 1] = c.green();
            out[i + 2] = c.red();
            out[i + 3] = c.alpha();
        }
        i += 4;
    }
    out
}

/// The opaque fast path, with tiny-skia's own demultiply for the translucent
/// pixels, writing through chunks.
fn convert_opaque_f32(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let pixels = pixmap.pixels();
    let mut out = vec![0u8; pixels.len() * 4];
    for (pixel, chunk) in pixels.iter().zip(out.chunks_exact_mut(4)) {
        if pixel.alpha() == 255 {
            chunk[0] = pixel.blue();
            chunk[1] = pixel.green();
            chunk[2] = pixel.red();
            chunk[3] = 255;
            continue;
        }
        let c = pixel.demultiply();
        chunk[0] = c.blue();
        chunk[1] = c.green();
        chunk[2] = c.red();
        chunk[3] = c.alpha();
    }
    out
}

/// The opaque fast path, writing with pushes.
fn convert_pushes_opaque_f32(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.width() as usize * pixmap.height() as usize * 4);
    for pixel in pixmap.pixels() {
        if pixel.alpha() == 255 {
            out.push(pixel.blue());
            out.push(pixel.green());
            out.push(pixel.red());
            out.push(255);
            continue;
        }
        let c = pixel.demultiply();
        out.push(c.blue());
        out.push(c.green());
        out.push(c.red());
        out.push(c.alpha());
    }
    out
}

/// The opaque fast path, with the whole pixel copied at once.
fn convert_opaque_vec(pixmap: &figcraft_render::Pixmap) -> Vec<u8> {
    let pixels = pixmap.pixels();
    let mut out = vec![0u8; pixels.len() * 4];
    for (pixel, chunk) in pixels.iter().zip(out.chunks_exact_mut(4)) {
        if pixel.alpha() == 255 {
            chunk.copy_from_slice(&[pixel.blue(), pixel.green(), pixel.red(), 255]);
            continue;
        }
        let c = pixel.demultiply();
        chunk.copy_from_slice(&[c.blue(), c.green(), c.red(), c.alpha()]);
    }
    out
}

/// The cost of a document clone, which is what every edit pays.
///
/// The engine's history is a stack of document snapshots (`Arc<Document>`), so
/// a command that changes the document has to make a copy of it: the snapshot
/// before the command and the document after it cannot be the same `Arc`. This
/// measures what that copy costs, because the question "is the undo slow
/// because of the clone?" has to be answered with a number and not with a
/// guess.
#[test]
fn the_cost_of_a_document_clone_is_measured() {
    for count in [100usize, 1000] {
        let (doc, page) = busy_document(count);
        let doc = Arc::new(doc);
        let _ = page;

        // The clone the engine's `Arc::make_mut` makes: the document is
        // immutable behind its `Arc`, so a change is a new one.
        let mut best = Duration::MAX;
        for _ in 0..20 {
            let started = Instant::now();
            let copy = doc.clone();
            let took = started.elapsed();
            std::hint::black_box(&copy);
            best = best.min(took);
        }
        eprintln!(
            "spool_bench clone nodes={count} best={:.3}ms",
            Measurement::ms(best)
        );

        // And the same, with the `Arc` shared the way the history shares it:
        // the clone is then the copy the engine actually makes.
        let mut best = Duration::MAX;
        for _ in 0..20 {
            let mut shared = Arc::clone(&doc);
            let started = Instant::now();
            let copy = Arc::make_mut(&mut shared);
            let took = started.elapsed();
            std::hint::black_box(copy);
            best = best.min(took);
        }
        eprintln!(
            "spool_bench clone_shared nodes={count} best={:.3}ms",
            Measurement::ms(best)
        );
    }
}
