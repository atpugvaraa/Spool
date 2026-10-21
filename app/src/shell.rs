use gpui::{
    div, prelude::*, px, rgb, App, Context, Entity, Modifiers, Render, SharedString, Subscription,
    Window,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::{commands, inspector, project_open, theme, tools};

/// The tools the toolbar offers, in the order it shows them.
///
/// `pub(crate)` so the window-level interaction tests can enumerate exactly what
/// the toolbar renders rather than keeping a second list that could drift.
pub(crate) const TOOLS: [(&str, &str, &str, tools::Tool); 7] = [
    ("↖", "Select", "V", tools::Tool::Select),
    ("▱", "Frame", "F", tools::Tool::Frame),
    ("□", "Rectangle", "R", tools::Tool::Rectangle),
    ("○", "Ellipse", "O", tools::Tool::Ellipse),
    ("⌁", "Pen", "P", tools::Tool::Pen),
    ("T", "Text", "T", tools::Tool::Text),
    ("◌", "Comment", "C", tools::Tool::Comment),
];

const PAGES: [&str; 4] = ["Landing", "App", "Components", "Explorations"];

/// The engine's name for one of Spool's tools.
///
/// The toolbar is written in Spool's tool names because that is what the
/// product is; the engine has its own, and the mapping is here rather than in
/// the engine because the engine's names are the upstream's, not Spool's to
/// change. `Select` is the engine's `move`: the select tool IS the move tool
/// with the pointer, and the engine's tool table has no separate select.
pub(crate) fn engine_tool_name(tool: tools::Tool) -> &'static str {
    match tool {
        tools::Tool::Select => "move",
        tools::Tool::Frame => "frame",
        tools::Tool::Rectangle => "rectangle",
        tools::Tool::Ellipse => "ellipse",
        tools::Tool::Pen => "pen",
        tools::Tool::Text => "text",
        tools::Tool::Comment => "comment",
    }
}

/// Which project the application was asked to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectRequest {
    /// A `.spool` project named on the command line. The product's own path.
    Project(PathBuf),
    /// A directory named by `SPOOL_PROJECT`.
    ///
    /// A development and testing override, not a product surface. It accepts any
    /// directory holding a `lamine.yaml`, which is how the committed fixtures are
    /// used, and it deliberately does *not* require the `.spool` suffix. Nothing
    /// a user can reach should depend on this.
    DevelopmentOverride(PathBuf),
}

/// Decide which project this launch asked for.
///
/// The command line wins over the development override: a person who typed a path
/// meant that path, and an inherited `SPOOL_PROJECT` should not quietly redirect
/// it.
///
/// Pure, so the three states the application has to tell apart — a project, an
/// unusable path, and nothing at all — can be decided in a test without a window.
fn project_request(
    requested: Option<PathBuf>,
    development_override: Option<PathBuf>,
) -> Option<ProjectRequest> {
    requested
        .map(ProjectRequest::Project)
        .or_else(|| development_override.map(ProjectRequest::DevelopmentOverride))
}

/// `SPOOL_PROJECT`, if it is set to something.
///
/// A development and testing override. See [`ProjectRequest::DevelopmentOverride`].
fn development_override() -> Option<PathBuf> {
    std::env::var_os("SPOOL_PROJECT")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Load the requested project.
///
/// Returns the message to show the user when the project could not be opened.
/// Three outcomes, kept distinct because they mean different things:
///
/// - **no project requested** — the starter scene, which is a normal state.
/// - **a project that opened** — compiled into the engine's document.
/// - **a project that did not open** — an error, reported on stderr *and* shown
///   in the window. The starter scene is still what is on screen, but it is
///   never presented as though it were the project: a named project that failed
///   to open has failed, and saying so is the whole point of reporting it.
fn load_requested_project(
    request: Option<&ProjectRequest>,
) -> (
    Option<SharedString>,
    Option<crate::engine_project::EngineProject>,
    Option<PathBuf>,
) {
    let Some(request) = request else {
        return (None, None, None);
    };
    let (root, opened) = match request {
        ProjectRequest::Project(path) => (
            path.display().to_string(),
            crate::spool_project::open(path).map_err(|error| error.to_string()),
        ),
        ProjectRequest::DevelopmentOverride(path) => (
            path.display().to_string(),
            project_open::open_project(path).map_err(|error| error.to_string()),
        ),
    };
    match opened {
        Ok(loaded) => {
            // Reported so a launch log shows what actually opened, rather than
            // leaving the operator to infer it from pixels.
            eprintln!(
                "spool_project_open ok root={} persistent_nodes={} engine_nodes={} unrendered={:?} unresolved={:?}",
                loaded.root.display(),
                loaded.document.structure.nodes.len(),
                crate::engine_project::content_node_count(&loaded.engine.document),
                loaded.unrendered,
                loaded.engine.unresolved,
            );
            let engine = loaded.engine.clone();
            let root = loaded.root.clone();
            (None, Some(engine), Some(root))
        }
        Err(error) => {
            eprintln!("spool_project_open failed root={root}: {error}");
            (
                Some(format!("Could not open {root}: {error}").into()),
                None,
                None,
            )
        }
    }
}

/// The open "New Project" prompt.
///
/// A typed path and nothing else. There is no folder picker to open: GPUI has no
/// file dialog, and adding a native one for this would mean a platform
/// dependency and a second way to choose a path — for a workflow whose whole
/// input is one string. The path is typed, so the name is the directory's name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NewProjectPrompt {
    buffer: String,
    select_all: bool,
}

/// What a key did to an open New Project prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PromptEffect {
    /// The path changed; the prompt is still open.
    Continue,
    /// Create the project at this path.
    Commit(PathBuf),
    /// Close the prompt without creating anything.
    Abandon,
}

impl NewProjectPrompt {
    fn opening() -> Self {
        Self {
            buffer: String::new(),
            select_all: true,
        }
    }

    /// Apply one keystroke.
    ///
    /// A modifier combination is an editor shortcut rather than a character, and
    /// is swallowed rather than typed, exactly as the Inspector's rename does —
    /// otherwise `⌘S` would type an `s` into the path.
    fn apply_key(&mut self, key: &str) -> PromptEffect {
        match key {
            "enter" | "return" => {
                let path = self.buffer.trim().to_owned();
                if path.is_empty() {
                    return PromptEffect::Continue;
                }
                let path = PathBuf::from(shellexpand_home(&path));
                self.buffer.clear();
                self.select_all = true;
                PromptEffect::Commit(path)
            }
            "escape" => {
                self.buffer.clear();
                self.select_all = true;
                PromptEffect::Abandon
            }
            "backspace" => {
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                } else {
                    self.buffer.pop();
                }
                PromptEffect::Continue
            }
            other if other.chars().count() == 1 => {
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                }
                self.buffer.push_str(other);
                PromptEffect::Continue
            }
            _ => PromptEffect::Continue,
        }
    }
}

/// The home directory with a trailing separator, for the prompt's placeholder.
fn home_directory_prefix() -> String {
    match std::env::var("HOME") {
        Ok(home) => format!("{home}/"),
        Err(_) => String::from("./"),
    }
}

/// Expand a leading `~`, so a typed path can start with one.
///
/// The only shell expansion worth having: everything else in a project location
/// is written out in full, and expanding the rest would mean running a shell to
/// interpret a string that names a directory.
fn shellexpand_home(path: &str) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_owned();
    };
    // `~/` and a bare `~` both mean the home directory; `~someone` is not ours to
    // expand and is left alone rather than guessed at.
    if !rest.is_empty() && !rest.starts_with('/') {
        return path.to_owned();
    }
    match std::env::var("HOME") {
        Ok(home) => format!("{home}{rest}"),
        Err(_) => path.to_owned(),
    }
}

/// The editor shell.
///
/// The shell arranges the three surfaces and owns the editor-wide keyboard: which
/// keys are shortcuts, and the order Escape unwinds them in. Everything about
/// showing and changing a selection belongs to the Inspector, which holds its own
/// interaction state for exactly that reason — there is no second copy of a
/// number, a scrub or a layer name anywhere in here.
pub struct AppShell {
    /// The directory the open project came from.
    ///
    /// Kept here because the save path needs it. The save writes the ENGINE's
    /// document back to these files, so this is the one thing the shell must
    /// still know about the project on disk.
    project_root: Option<PathBuf>,
    /// The editor over the figcraft engine. This is the editor: the retired
    /// hand-written runtime is gone, and the engine's document is the only
    /// model of the open project.
    engine_canvas: Entity<crate::engine::canvas::EngineCanvas>,
    /// The engine's layers panel, sharing the one session with `engine_canvas`.
    engine_layers: Entity<crate::engine::layers::LayersPanel>,
    inspector: Entity<inspector::Inspector>,
    selected_page: usize,
    ai_open: bool,
    share_open: bool,
    export_open: bool,
    zoom_open: bool,
    /// The open New Project prompt, when one is.
    new_project: Option<NewProjectPrompt>,
    /// A message shown over the canvas, and whether it reports a failure.
    ///
    /// Carries the result of the last save and the result of opening the
    /// requested project. Both have to be visible for the same reason: a save
    /// that silently dropped an edit, or a project that silently failed to open
    /// and left the starter scene on screen, would each look identical to one
    /// that succeeded.
    save_status: Option<(SharedString, bool)>,
    /// Repaint subscriptions that must outlive this function.
    ///
    /// A dropped `Subscription` detaches its observer, so the shell keeps them
    /// for its whole life: the two engine panels share one session and cannot
    /// see each other's edits, so this is where cross-panel invalidation
    /// happens.
    /// Nothing reads this field; holding it is the point.
    #[allow(dead_code)]
    observations: Vec<Subscription>,
}

impl AppShell {
    /// Open the project named on the command line.
    ///
    /// `requested` is the path from the process arguments. `None` — a launch with
    /// no argument — is the ordinary empty state, and is also what the tests and
    /// the `SPOOL_PROJECT` override rely on.
    pub fn new_with_project(requested: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let request = project_request(requested, development_override());
        let (message, engine, root) = load_requested_project(request.as_ref());
        let status = message.map(|message| (message, true));
        let inspector = cx.new(|_| inspector::Inspector::new());

        // The engine frontend. ONE session is built here and shared: the canvas
        // owns it, and the layers panel takes the same `Rc<RefCell<Session>>`.
        // Two sessions would be two documents that disagree, which is the one
        // thing this architecture exists to prevent.
        let document = engine.map_or_else(Default::default, |engine| engine.document);
        let session = figcraft_engine::Session::with_document(document);
        let engine_canvas = cx.new(|cx| crate::engine::canvas::EngineCanvas::new(session, cx));
        // Frame the content, deferred to the first paint that knows the canvas's
        // size. Without this the camera sits at the default origin looking at
        // empty space and a correctly compiled project opens to a black canvas.
        engine_canvas.update(cx, |canvas, _| canvas.fit_on_next_paint());
        let session = engine_canvas.update(cx, |canvas, _| canvas.session_handle());
        let engine_layers = cx.new(|cx| crate::engine::layers::LayersPanel::new(session, cx));

        // The Inspector is built before the engine canvas exists, so it is
        // joined to it afterwards. It reads the engine's document and the
        // engine's selection; without this it would show "No selection" for
        // ever, because nothing else holds a selection.
        inspector.update(cx, |inspector, cx| {
            inspector.set_engine_canvas(engine_canvas.clone(), cx);
        });

        // Held, not dropped: a GPUI subscription detaches its observer when it
        // goes away, and without it the chrome around the canvas would only
        // repaint when the shell itself changed.
        //
        // The engine panels have no observer of their own — two entities sharing
        // one `Rc<RefCell<Session>>` cannot see each other's borrows — so this is
        // the only place cross-panel invalidation happens.
        //
        // Each panel notifies the other ONLY when the document revision actually
        // moved. Notifying unconditionally is a livelock, not a redundancy: in
        // GPUI a notification IS an observation, so two entities that observe
        // each other and always notify wake each other forever and the window
        // never appears. That is not a theoretical concern — it is what the
        // first version of this did, and `sample` showed the whole main thread
        // inside shell.rs recursing through invoke_observer -> notify.
        //
        // `seen` is shared by both observers so the pair cannot disagree about
        // what the last propagated revision was; two separate cells would let a
        // lost update leave the panels a revision apart with nothing to wake
        // them again.
        let seen: Rc<Cell<u64>> = Rc::new(Cell::new(u64::MAX));
        let observations = vec![
            cx.observe(&engine_canvas, {
                let seen = Rc::clone(&seen);
                move |shell, canvas, cx| {
                    let revision = canvas.update(cx, |canvas, _| canvas.document_revision());
                    if seen.replace(revision) != revision {
                        shell.engine_layers.update(cx, |_, cx| cx.notify());
                    }
                }
            }),
            cx.observe(&engine_layers, {
                let seen = Rc::clone(&seen);
                move |shell, layers, cx| {
                    let revision = layers.update(cx, |layers, _| layers.document_revision());
                    if seen.replace(revision) != revision {
                        shell.engine_canvas.update(cx, |_, cx| cx.notify());
                    }
                }
            }),
        ];
        Self {
            project_root: root,
            engine_canvas,
            engine_layers,
            inspector,
            observations,
            selected_page: 0,
            new_project: None,
            ai_open: false,
            share_open: false,
            export_open: false,
            zoom_open: false,
            save_status: status,
        }
    }

    /// Escape, as one ladder walked from the top.
    ///
    /// What Escape cancels must never depend on which listener happened to see
    /// the key first, so this is the only place that decides and it decides by
    /// reading [`commands::EscapeState`]: the ladder and its order live there,
    /// where they can be tested without a window, and this function is the only
    /// place that acts on the rung it returns.
    ///
    /// The order is the finding, not a preference. Figma, tldraw and Affinity
    /// all treat Escape as "up one level" — a selection scope, a state chart, a
    /// tool mode — and Canva treats it as cancel:
    ///
    /// 1. an open panel, because a modal is above everything
    /// 2. the Inspector, which walks its own rungs in the same order: a text
    ///    buffer before a gesture, because what the user typed into is above
    ///    what they are dragging
    /// 3. a canvas gesture
    /// 4. an open text session
    /// 5. the selection
    /// 6. a non-Select tool, which returns to Select
    ///
    /// Returns whether anything was cancelled, so the caller knows the key was
    /// consumed.
    fn escape_topmost(&mut self, cx: &mut Context<Self>) -> bool {
        // Whatever the Inspector is part-way through — a number being typed, a
        // name being typed, or a drag in flight — is read as one rung. The
        // panel walks its own rungs internally, in the same order, so the two
        // cannot disagree about which of its two states is on top.
        //
        // The rungs are read from the ENGINE, because the engine is what holds
        // the document, the selection, the tool and the text session now.
        let (gesture, text_editing, has_selection, tool_is_held) = {
            let engine_canvas = self.engine_canvas.read(cx);
            (
                engine_canvas.busy(),
                engine_canvas.is_editing_text(),
                !engine_canvas.selection().is_empty(),
                engine_canvas.active_tool() != "move",
            )
        };
        let state = commands::EscapeState {
            panel: self.ai_open || self.share_open || self.export_open || self.zoom_open,
            inspector: self
                .inspector
                .update(cx, |inspector, _| inspector.is_busy()),
            gesture,
            text_editing,
            selection: has_selection,
            tool: tool_is_held,
        };
        match state.next() {
            Some(commands::Rung::Panel) => {
                self.ai_open = false;
                self.share_open = false;
                self.export_open = false;
                self.zoom_open = false;
                cx.notify();
                true
            }
            Some(commands::Rung::Inspector) => self
                .inspector
                .update(cx, |inspector, cx| inspector.cancel_in_flight(cx)),
            // The engine's own escape: it knows whether a gesture is in flight
            // and whether a text session is open, and it cancels whichever is
            // actually there.
            Some(commands::Rung::Gesture) | Some(commands::Rung::TextEditing) => {
                self.engine_canvas.update(cx, |canvas, cx| {
                    canvas.escape(cx);
                });
                true
            }
            Some(commands::Rung::Selection) => {
                self.run_engine_command("select.none", serde_json::json!({}), cx);
                true
            }
            Some(commands::Rung::Tool) => {
                // Escape returns to the select tool, and the select tool is the
                // engine's `move` tool. It has to be the engine's, because the
                // engine canvas is what the user is now driving.
                self.select_engine_tool("move", cx);
                true
            }
            None => false,
        }
    }

    /// Arrow keys: nudge a selection, otherwise pan the canvas.
    ///
    /// Figma's rule, recorded in `navigation.md`: with nothing selected the
    /// arrow keys pan, and with a selection they nudge it by 1px, or 10px with
    /// `⇧`. One press is one semantic operation, so it is one undo step — which
    /// is what makes holding an arrow key cheap to undo.
    ///
    /// Which of the two a key means is a fact about the document, not about the
    /// key, so it is read here rather than in
    /// [`commands::resolve`](commands::resolve): the resolver can only say "an
    /// arrow key was pressed".
    fn arrow_key(
        &mut self,
        direction: commands::Direction,
        coarse: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let (dx, dy) = match direction {
            commands::Direction::Left => (-1.0, 0.0),
            commands::Direction::Right => (1.0, 0.0),
            commands::Direction::Up => (0.0, -1.0),
            commands::Direction::Down => (0.0, 1.0),
        };
        // The engine's nudge takes UNITS, not pixels: it multiplies them by its
        // own small/big preference, so the step size is the engine's decision
        // and not a second copy of it here.
        let moved = self.engine_canvas.update(cx, |canvas, cx| {
            canvas.run_engine_command(
                "transform.nudge",
                serde_json::json!({ "dx": dx, "dy": dy, "big": coarse }),
                cx,
            )
        });
        if moved {
            return true;
        }
        // Nothing selected: the same keys move the view instead. A bigger jump,
        // because panning is navigation rather than an edit and does not need
        // the fine granularity a nudge does.
        let pan_step = if coarse { 400.0 } else { 40.0 };
        self.engine_canvas.update(cx, |canvas, cx| {
            canvas.pan_by(dx * pan_step, dy * pan_step, cx);
        });
        cx.notify();
        true
    }

    /// Apply one key to an open New Project prompt.
    fn handle_new_project_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(prompt) = self.new_project.as_mut() else {
            return;
        };
        match prompt.apply_key(key) {
            PromptEffect::Continue => {}
            PromptEffect::Abandon => self.new_project = None,
            PromptEffect::Commit(path) => {
                self.new_project = None;
                self.create_project(path, cx);
            }
        }
    }

    /// Create a project at `path` and open it, or say why it could not.
    ///
    /// Creation and opening are one step from here on purpose. The new project is
    /// opened through [`load_requested_project`] — the same path a command-line
    /// launch takes — rather than a second one, so there is still exactly one way
    /// a project reaches the editor.
    ///
    /// A failure is reported and nothing is loaded. Leaving the previous document
    /// on screen after a failed create would present it as though it were the
    /// project that was just asked for.
    fn create_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let project = match crate::spool_project::create(&path) {
            Ok(project) => project,
            Err(error) => {
                eprintln!(
                    "spool_project_create failed path={}: {error}",
                    path.display()
                );
                self.save_status = Some((
                    format!("Could not create {}: {error}", path.display()).into(),
                    true,
                ));
                return;
            }
        };
        eprintln!("spool_project_create ok root={}", project.root().display());
        let request = ProjectRequest::Project(path.clone());
        let (message, engine, root) = load_requested_project(Some(&request));
        self.project_root = root;
        if let Some(message) = message {
            self.save_status = Some((message, true));
        }
        if let Some(engine) = engine {
            self.load_engine_document(engine.document, cx);
        }
    }

    /// Put a freshly opened document into the session both engine panels share.
    ///
    /// The panels hold an `Rc<RefCell<Session>>`, not a `Weak` to an entity, so a
    /// new document is installed by replacing the session's document in place.
    /// Swapping the whole `Session` instead would leave the layers panel holding
    /// the old one, and the two would disagree about what is open.
    fn load_engine_document(&mut self, document: figcraft_doc::Document, cx: &mut Context<Self>) {
        let session = self
            .engine_canvas
            .update(cx, |canvas, _| canvas.session_handle());
        {
            let mut session = session.borrow_mut();
            session.docs.clear();
            session
                .docs
                .push(figcraft_engine::DocState::new(document, None));
            session.active = 0;
        }
        self.engine_canvas.update(cx, |canvas, cx| {
            canvas.fit_on_next_paint();
            cx.notify();
        });
        self.engine_layers.update(cx, |_, cx| cx.notify());
    }

    /// The one place a keystroke the whole editor answers to is handled.
    ///
    /// Three steps, always in this order, and the order is the whole point: what
    /// a key means is decided by which surface is above, never by which listener
    /// happened to run first.
    ///
    /// 1. The Inspector gets first refusal. A number or a name being typed is
    ///    not an editor shortcut, so letters go into the field rather than into
    ///    the tool shortcuts. It declines Escape, which is the ladder's.
    /// 2. Escape walks [`commands::EscapeState`] from the top. It is not in the
    ///    command table, because "leave the most recent thing you entered" is
    ///    not one of the table's verbs.
    /// 3. Everything else is one row of [`commands::resolve`].
    fn handle_key(
        &mut self,
        key: &str,
        modifiers: Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // An open prompt owns the keyboard outright: it is the only surface
        // accepting a path right now, so letting the Inspector or an editor
        // shortcut answer first would type into the wrong place.
        if self.new_project.is_some() {
            self.handle_new_project_key(key, cx);
            return;
        }
        if self
            .inspector
            .update(cx, |inspector, cx| inspector.handle_key(key, modifiers, cx))
        {
            return;
        }
        if key == "escape" {
            self.escape_topmost(cx);
            return;
        }
        let scope = if self.engine_canvas.read(cx).is_editing_text() {
            commands::Scope::TextEditing
        } else {
            commands::Scope::Editor
        };
        let Some(command) = commands::resolve(key, modifiers, scope) else {
            return;
        };
        // While the engine canvas owns the keyboard, the engine is the
        // authority: its own keymap dispatches figcraft command ids, and its
        // gesture handling answers the keys that are not commands. The shell's
        // command table must not answer the same chord as well, or one Cmd+Z
        // would undo twice -- once in each runtime.
        //
        // THE SAVE IS THE EXCEPTION. Spool's save writes the engine's document
        // back to the authored source, and the engine has no path there: its
        // own `file.save` writes the engine's document format, which is a
        // different file, and its binding is deliberately not installed (see
        // `engine::keymap`). So the shell answers the save chord itself,
        // whether or not the canvas holds the focus.
        if self.engine_canvas.read(cx).has_focus(window)
            && !matches!(command, commands::Command::Save)
        {
            return;
        }
        self.run_command(command, cx);
    }

    /// Choose a tool, on the engine.
    ///
    /// The toolbar's buttons go through here, and so does every shortcut: the
    /// engine is what the user is editing. The name is the engine's own, so
    /// this is the same call a shortcut, the CLI or an agent would make.
    fn select_engine_tool(&mut self, tool: &str, cx: &mut Context<Self>) {
        self.engine_canvas
            .update(cx, |canvas, cx| canvas.set_tool(tool, cx));
    }

    /// Whether the engine's tool in effect is `tool`.
    ///
    /// The engine's answer, read from the engine, so the highlighted tool in the
    /// toolbar cannot drift from the tool the canvas is actually using.
    fn engine_tool_is_highlighted(&self, tool: &str, cx: &App) -> bool {
        self.engine_canvas.read(cx).active_tool() == tool
    }

    /// The engine's tool in effect, for the window-level interaction tests.
    #[cfg(test)]
    pub fn test_engine_tool(&self, cx: &App) -> &'static str {
        self.engine_canvas.read(cx).active_tool()
    }

    /// The engine canvas entity, for the window-level interaction tests.
    #[cfg(test)]
    pub fn engine_canvas(&self) -> &Entity<crate::engine::canvas::EngineCanvas> {
        &self.engine_canvas
    }

    /// The Inspector entity, for the window-level interaction tests.
    #[cfg(test)]
    pub fn inspector(&self) -> &Entity<inspector::Inspector> {
        &self.inspector
    }

    /// The layers panel entity, for the window-level interaction tests.
    #[cfg(test)]
    pub fn engine_layers(&self) -> &Entity<crate::engine::layers::LayersPanel> {
        &self.engine_layers
    }

    /// Whether the engine's tool in effect is `tool`, for the tests.
    #[cfg(test)]
    pub fn test_engine_tool_is_highlighted(&self, tool: &str, cx: &App) -> bool {
        self.engine_tool_is_highlighted(tool, cx)
    }

    /// The message over the canvas and whether it reports a failure, for the
    /// window-level interaction tests.
    #[cfg(test)]
    pub fn status_message(&self) -> Option<(String, bool)> {
        self.save_status
            .as_ref()
            .map(|(message, failed)| (message.to_string(), *failed))
    }

    /// Carry out one editor-wide command.
    ///
    /// Every history-affecting arm below goes through the engine, which commits
    /// to the one history the pointer uses. There is no second route from the
    /// keyboard to the document, and no arm that edits state directly: a
    /// command that mutated here would be invisible to undo.
    fn run_command(&mut self, command: commands::Command, cx: &mut Context<Self>) {
        match command {
            commands::Command::Undo => {
                self.engine_canvas.update(cx, |canvas, cx| {
                    canvas.undo(cx);
                });
            }
            commands::Command::Redo => {
                self.engine_canvas.update(cx, |canvas, cx| {
                    canvas.redo(cx);
                });
            }
            commands::Command::Save => self.save_project(cx),
            commands::Command::Delete => {
                self.run_engine_command("edit.delete", serde_json::json!({}), cx);
            }
            commands::Command::Duplicate => {
                self.run_engine_command("edit.duplicate", serde_json::json!({}), cx);
            }
            commands::Command::SelectAll => {
                self.run_engine_command("select.all", serde_json::json!({}), cx);
            }
            commands::Command::Rename => self.rename_selection(cx),
            commands::Command::Tool(tool) => {
                self.select_engine_tool(engine_tool_name(tool), cx);
            }
            commands::Command::Nudge { direction, coarse } => {
                self.arrow_key(direction, coarse, cx);
            }
            commands::Command::Traverse(step) => {
                // The engine's selection ladder: next/previous sibling, into the
                // children, out to the parent. One command per rung, so the
                // ladder is the engine's and not a second list here.
                let id = match step {
                    tools::Traversal::Sibling(next) => {
                        if next {
                            "select.next"
                        } else {
                            "select.previous"
                        }
                    }
                    tools::Traversal::Descend => "select.children",
                    tools::Traversal::Ascend => "select.parent",
                };
                self.run_engine_command(id, serde_json::json!({}), cx);
            }
            commands::Command::ZoomIn => {
                self.run_engine_command("view.zoomIn", serde_json::json!({}), cx);
            }
            commands::Command::ZoomOut => {
                self.run_engine_command("view.zoomOut", serde_json::json!({}), cx);
            }
            commands::Command::ZoomToFit => {
                self.run_engine_command("view.zoomFit", serde_json::json!({}), cx);
            }
            commands::Command::ZoomToSelection => {
                self.run_engine_command("view.zoomSelection", serde_json::json!({}), cx);
            }
            commands::Command::ZoomToActualSize => {
                self.run_engine_command("view.zoom100", serde_json::json!({}), cx);
            }
        }
        // A camera command moves no document state, so the chrome around the
        // canvas has to be told even though the canvas already notified.
        if command.is_camera() {
            cx.notify();
        }
    }

    /// Run one engine command against the session both panels share.
    ///
    /// The engine's commands are the operation surface. A shortcut, a menu
    /// item, the command palette, the CLI and a future agent all dispatch
    /// command ids, so an edit made by a key and an edit made by a click are
    /// the same edit and land in the same history.
    fn run_engine_command(&mut self, id: &str, params: serde_json::Value, cx: &mut Context<Self>) {
        self.engine_canvas
            .update(cx, |canvas, cx| canvas.run_engine_command(id, params, cx));
    }

    /// Open a rename on the selection — `Enter` or `F2`.
    ///
    /// The Inspector owns the one rename model, whichever surface asked for it:
    /// the Layers panel states a rename request and this states the same kind of
    /// request, so a name typed from the canvas is the same buffer, the same
    /// commit, and the same Escape behaviour as one typed from the panel.
    ///
    /// Only a single selection. Figma renames the layer it has selected and
    /// does nothing otherwise, and renaming forty layers into one string would
    /// not be a rename.
    fn rename_selection(&mut self, cx: &mut Context<Self>) {
        // The engine's selection, and the engine's identity.
        let ids = self.engine_canvas.read(cx).selection();
        let [id] = ids.as_slice() else {
            return;
        };
        self.inspector.update(cx, |inspector, cx| {
            inspector.open_rename(*id, cx);
            cx.notify();
        });
    }

    /// Save the open project and report what actually happened.
    ///
    /// Goes through the engine's document, so there is exactly one save path,
    /// and says what could not be written rather than implying everything
    /// persisted.
    fn save_project(&mut self, cx: &mut Context<Self>) {
        // The ENGINE's document is what is saved, because the engine is what the
        // user has been editing.
        let Some(root) = self.project_root.clone() else {
            self.save_status = Some(("No project open".into(), true));
            cx.notify();
            return;
        };
        let document = {
            let canvas = self.engine_canvas.read(cx);
            canvas.with_session(|session| session.doc().ok().cloned())
        };
        let Some(document) = document else {
            self.save_status = Some(("Save failed: no open document".into(), true));
            cx.notify();
            return;
        };
        let status = match crate::engine::source_edits::save_engine_document(&root, &document) {
            Ok(outcome)
                if outcome.written.is_empty()
                    && outcome.unsupported.is_empty()
                    && outcome.notes.is_empty() =>
            {
                "No changes to save".to_owned()
            }
            Ok(outcome) => {
                let mut parts = Vec::new();
                if !outcome.written.is_empty() {
                    parts.push(format!("Saved {} file(s)", outcome.written.len()));
                }
                if !outcome.unsupported.is_empty() {
                    let names: Vec<&str> = outcome
                        .unsupported
                        .iter()
                        .map(|edit| edit.node.as_str())
                        .collect();
                    parts.push(format!("not saved: {}", names.join(", ")));
                }
                // The notes are project-level facts the save wants the user to
                // know -- that the metadata is version 2 and was left alone, for
                // instance. They are not failures, so they are not counted as
                // such.
                parts.extend(outcome.notes.iter().cloned());
                if parts.is_empty() {
                    "No changes to save".to_owned()
                } else {
                    parts.join("; ")
                }
            }
            Err(error) => format!("Save failed: {error}"),
        };
        let failed = status.starts_with("Save failed");
        eprintln!("spool_save_command {status}");
        self.save_status = Some((status.into(), failed));
        cx.notify();
    }

    /// The New Project affordance, and the prompt when one is open.
    ///
    /// Deliberately a button and a typed path. GPUI has no file dialog, and this
    /// milestone is not the place to add a platform dependency and a second way
    /// to choose a directory; the project's location is one string and is typed.
    fn new_project_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = div()
            .id("new-project-button")
            .flex()
            .items_center()
            .gap_1()
            .px(px(9.0))
            .py(px(5.0))
            .rounded_md()
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.new_project = match this.new_project.take() {
                    Some(_) => None,
                    None => Some(NewProjectPrompt::opening()),
                };
                cx.notify();
            }))
            .child("+");
        let button = div().relative().child(button);
        if self.new_project.is_none() {
            return button;
        }
        div()
            .relative()
            .child(button)
            .child(self.new_project_prompt())
    }

    /// The open prompt: the path being typed, and what Enter will do with it.
    fn new_project_prompt(&self) -> impl IntoElement {
        let buffer = self
            .new_project
            .as_ref()
            .map(|prompt| prompt.buffer.as_str())
            .unwrap_or_default();
        let shown = if buffer.is_empty() {
            format!("{}MyProject.spool", home_directory_prefix())
        } else {
            buffer.to_owned()
        };
        div()
            .id("new-project-prompt")
            .absolute()
            .top(px(34.0))
            .left(px(0.0))
            .w(px(360.0))
            .p(px(10.0))
            .rounded_lg()
            .bg(rgb(theme::SURFACE_RAISED))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::TEXT))
                    .child("New Spool Project"),
            )
            .child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded_md()
                    .bg(rgb(theme::CANVAS))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_xs()
                    .text_color(if buffer.is_empty() {
                        rgb(theme::TEXT_MUTED)
                    } else {
                        rgb(theme::TEXT)
                    })
                    .child(shown),
            )
            .child(div().text_xs().text_color(rgb(theme::TEXT_MUTED)).child(
                "The directory name is the project name. Enter creates and opens it; Esc cancels.",
            ))
    }

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let left = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.0))
                    .rounded_md()
                    .bg(rgb(theme::ACCENT_WASH))
                    .text_color(rgb(theme::ACCENT))
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("S"),
            )
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::TEXT))
                    .child("Spool"),
            )
            .child(div().w(px(1.0)).h(px(22.0)).bg(rgb(theme::BORDER)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme::TEXT))
                            .child("Fieldnotes"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(format!("Website  /  {}", PAGES[self.selected_page])),
                    ),
            )
            .child(self.new_project_button(cx));

        let mut tools = div()
            .flex()
            .items_center()
            .gap(px(3.0))
            .p(px(4.0))
            .rounded_lg()
            .bg(rgb(theme::SURFACE))
            .border_1()
            .border_color(rgb(theme::BORDER));
        for (index, (icon, label, _shortcut, tool)) in TOOLS.iter().enumerate() {
            let selected = self.engine_tool_is_highlighted(engine_tool_name(*tool), cx);
            let background = if selected {
                theme::SURFACE_HOVER
            } else {
                theme::SURFACE
            };
            let foreground = if selected {
                theme::TEXT
            } else {
                theme::TEXT_SECONDARY
            };
            let tool = engine_tool_name(*tool);
            tools = tools.child(
                div()
                    .id(SharedString::from(format!("top-tool-{index}")))
                    // GPUI's hook for locating an element from a test; a
                    // no-op in release builds. Lets the interaction tests click
                    // the actual toolbar button instead of calling the command
                    // the button is bound to.
                    .debug_selector(|| format!("tool-{label}"))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(9.0))
                    .py(px(7.0))
                    .rounded_md()
                    .bg(rgb(background))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .active(|style| style.opacity(0.78))
                    .text_sm()
                    .text_color(rgb(foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_engine_tool(tool, cx);
                    }))
                    .child(*icon)
                    .child(div().text_xs().child(*label)),
            );
        }

        let mut share = div().relative().child(
            div()
                .id("share-button")
                .flex()
                .items_center()
                .gap_2()
                .px(px(12.0))
                .py(px(8.0))
                .rounded_md()
                .bg(rgb(theme::SURFACE_RAISED))
                .border_1()
                .border_color(rgb(theme::BORDER))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .active(|style| style.opacity(0.78))
                .text_sm()
                .text_color(rgb(theme::TEXT))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.share_open = !this.share_open;
                    this.export_open = false;
                    cx.notify();
                }))
                .child("↗")
                .child("Share"),
        );
        if self.share_open {
            share = share.child(share_popover());
        }

        let mut export = div().relative().child(
            div()
                .id("export-button")
                .flex()
                .items_center()
                .gap_2()
                .px(px(12.0))
                .py(px(8.0))
                .rounded_md()
                .bg(rgb(theme::SURFACE_RAISED))
                .border_1()
                .border_color(rgb(theme::BORDER))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .active(|style| style.opacity(0.78))
                .text_sm()
                .text_color(rgb(theme::TEXT))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.export_open = !this.export_open;
                    this.share_open = false;
                    cx.notify();
                }))
                .child("⇧")
                .child("Export"),
        );
        if self.export_open {
            export = export.child(export_popover());
        }

        let agent = div()
            .id("agent-toggle")
            .flex()
            .items_center()
            .gap_2()
            .px(px(12.0))
            .py(px(8.0))
            .rounded_md()
            .bg(rgb(theme::ACCENT_WASH))
            .hover(|style| style.bg(rgb(0x343954)))
            .active(|style| style.opacity(0.78))
            .text_sm()
            .text_color(rgb(theme::TEXT))
            .on_click(cx.listener(|this, _, _, cx| {
                this.ai_open = !this.ai_open;
                cx.notify();
            }))
            .child("✦")
            .child(if self.ai_open {
                "Hide agent"
            } else {
                "Ask Spool"
            });

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(62.0))
            .px(px(14.0))
            .bg(rgb(theme::WINDOW))
            .border_b_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .child(left)
            .child(tools)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.zoom_control(cx))
                    .child(share)
                    .child(export)
                    .child(agent),
            )
    }

    fn zoom_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The engine's own camera, so the readout cannot drift from what the
        // canvas is actually showing.
        let label = format!(
            "{}%",
            (self.engine_canvas.read(cx).view().zoom * 100.0).round() as i64
        );
        let mut control = div().relative().child(
            div()
                .id("zoom-control")
                .flex()
                .items_center()
                .gap_2()
                .px(px(10.0))
                .py(px(8.0))
                .rounded_md()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.zoom_open = !this.zoom_open;
                    this.share_open = false;
                    this.export_open = false;
                    cx.notify();
                }))
                .child(label)
                .child("⌄"),
        );
        if self.zoom_open {
            control = control.child(self.zoom_menu(cx));
        }
        control
    }

    fn zoom_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .top(px(38.0))
            .right(px(0.0))
            .flex()
            .flex_col()
            .gap_1()
            .w(px(116.0))
            .p(px(6.0))
            .rounded_lg()
            .bg(rgb(theme::SURFACE_RAISED))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .shadow_lg()
            .child(self.zoom_option("25%", Some(25), cx))
            .child(self.zoom_option("50%", Some(50), cx))
            .child(self.zoom_option("100%", Some(100), cx))
            .child(self.zoom_option("200%", Some(200), cx))
            .child(div().h(px(1.0)).my(px(3.0)).bg(rgb(theme::BORDER_SOFT)))
            .child(self.zoom_option("Fit", None, cx))
    }

    fn zoom_option(
        &self,
        label: &'static str,
        zoom_percent: Option<u32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("zoom-option-{label}")))
            .px(px(9.0))
            .py(px(8.0))
            .rounded_md()
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .on_click(cx.listener(move |this, _, _, cx| {
                // The engine's own camera commands, so the zoom a menu item
                // asks for is the zoom the engine's keymap and the CLI ask
                // for.
                match zoom_percent {
                    Some(percent) => {
                        this.run_engine_command(
                            "view.set",
                            serde_json::json!({ "zoom": f64::from(percent) / 100.0 }),
                            cx,
                        );
                    }
                    None => {
                        this.run_engine_command("view.zoomFit", serde_json::json!({}), cx);
                    }
                }
                this.zoom_open = false;
                cx.notify();
            }))
            .child(label)
    }

    fn left_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut pages = div().flex().flex_col().gap_1().px(px(10.0)).pb(px(14.0));
        for (index, page) in PAGES.iter().enumerate() {
            let selected = self.selected_page == index;
            let background = if selected {
                theme::SURFACE_HOVER
            } else {
                theme::SURFACE
            };
            let foreground = if selected {
                theme::TEXT
            } else {
                theme::TEXT_SECONDARY
            };
            pages = pages.child(
                div()
                    .id(SharedString::from(format!("page-{index}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(30.0))
                    .px(px(9.0))
                    .rounded_md()
                    .bg(rgb(background))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .text_sm()
                    .text_color(rgb(foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_page = index;
                        cx.notify();
                    }))
                    .child(if selected { "◉" } else { "○" })
                    .child(*page),
            );
        }

        // Not `.cached(..)`: the engine panel paints its own rows, and caching
        // it inside a `cached_style` element has nothing to cache.
        let tree = self.engine_layers.clone();

        div()
            .flex()
            .flex_col()
            .w(px(204.0))
            .flex_shrink_0()
            .bg(rgb(theme::SURFACE))
            .border_r_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .pt(px(15.0))
                    .pb(px(9.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child("PAGES"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child("+"),
                    ),
            )
            .child(pages)
            .child(div().h(px(1.0)).bg(rgb(theme::BORDER_SOFT)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .pt(px(14.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child("LAYERS"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child("+"),
                    ),
            )
            .child(tree)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .justify_end()
                    .p(px(12.0))
                    .child(
                        div()
                            .id("prototype-workspace")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px(px(9.0))
                            .py(px(8.0))
                            .rounded_md()
                            .bg(rgb(theme::WINDOW))
                            .border_1()
                            .border_color(rgb(theme::BORDER_SOFT))
                            .cursor_pointer()
                            // The Prototype workspace opens the Prototype tab,
                            // which is a MODE as well as a panel: the canvas
                            // draws the prototype's connections while it is
                            // open.
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector.update(cx, |inspector, cx| {
                                    inspector.open_tab(1, cx);
                                });
                            }))
                            .child(
                                div()
                                    .size(px(7.0))
                                    .rounded_full()
                                    .bg(rgb(theme::TEXT_MUTED)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme::TEXT_SECONDARY))
                                    .child("Prototype workspace"),
                            ),
                    ),
            )
    }

    fn tool_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut palette = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .w(px(58.0))
            .p(px(5.0))
            .rounded_lg()
            .bg(rgb(theme::SURFACE))
            .border_1()
            .border_color(rgb(theme::BORDER));
        for (index, (icon, label, _shortcut, tool)) in TOOLS.iter().enumerate() {
            let selected = self.engine_tool_is_highlighted(engine_tool_name(*tool), cx);
            let background = if selected {
                theme::ACCENT_WASH
            } else {
                theme::SURFACE
            };
            let foreground = if selected {
                theme::ACCENT
            } else {
                theme::TEXT_SECONDARY
            };
            let tool = engine_tool_name(*tool);
            palette = palette.child(
                div()
                    .id(SharedString::from(format!("palette-tool-{index}")))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.0))
                    .w(px(46.0))
                    .h(px(43.0))
                    .rounded_md()
                    .bg(rgb(background))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .active(|style| style.opacity(0.78))
                    .text_color(rgb(foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_engine_tool(tool, cx);
                    }))
                    .child(div().text_sm().child(*icon))
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(*label),
                    ),
            );
        }
        palette
    }

    fn canvas_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .flex_1()
            .relative()
            .overflow_hidden()
            .bg(rgb(theme::CANVAS))
            .child(self.engine_canvas.clone());

        if let Some((message, failed)) = &self.save_status {
            content = content.child(
                div()
                    .absolute()
                    .left(px(16.0))
                    .bottom(px(16.0))
                    .rounded_md()
                    .px(px(10.0))
                    .py(px(6.0))
                    .bg(rgb(0x1b1e24))
                    .text_color(rgb(if *failed { 0xff8a8a } else { 0xd8dbe2 }))
                    .text_xs()
                    .child(message.clone()),
            );
        }

        if self.ai_open {
            content = content.child(self.agent_panel(cx));
        }

        div()
            .flex()
            .flex_1()
            .gap_3()
            .p(px(12.0))
            .overflow_hidden()
            .bg(rgb(theme::CANVAS))
            .child(self.tool_palette(cx))
            .child(content)
    }

    fn agent_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .right(px(22.0))
            .bottom(px(20.0))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(340.0))
            .p(px(16.0))
            .rounded_lg()
            .bg(rgb(0x1b1e24))
            .border_1()
            .border_color(rgb(0x343944))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(27.0))
                                    .rounded_full()
                                    .bg(rgb(theme::ACCENT_WASH))
                                    .text_color(rgb(theme::ACCENT))
                                    .child("✦"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(rgb(theme::TEXT))
                                            .child("Spool Agent"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(theme::TEXT_MUTED))
                                            .child("Local model · not connected"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("close-agent")
                            .px(px(5.0))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .hover(|style| style.text_color(rgb(theme::TEXT)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ai_open = false;
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .text_size(px(17.0))
                    .line_height(px(22.0))
                    .text_color(rgb(theme::TEXT))
                    .child("What should we make better?"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(agent_suggestion("✳", "Improve the selected section"))
                    .child(agent_suggestion("↗", "Create a responsive version"))
                    .child(agent_suggestion("◇", "Explore a visual direction")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(11.0))
                    .py(px(9.0))
                    .rounded_md()
                    .bg(rgb(theme::WINDOW))
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .text_sm()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("Ask about this design…")
                    .child(
                        div()
                            .size(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(theme::ACCENT_WASH))
                            .text_color(rgb(theme::ACCENT))
                            .child("↑"),
                    ),
            )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .overflow_hidden()
            .child(self.left_sidebar(cx))
            .child(self.canvas_area(cx))
            // The Inspector is its own view: it reads the canvas, holds its own
            // interaction state, and repaints itself when the document changes.
            // Mounting it is the whole integration — the shell has no say in
            // what a property row looks like or what a drag means.
            .child(self.inspector.clone())
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::diagnostics::count("shell_render", 1);
        let render_start = crate::diagnostics::start();
        let element = div()
            .id("spool-shell")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::WINDOW))
            .text_color(rgb(theme::TEXT))
            // The engine's keymap dispatches figcraft command ids as GPUI
            // actions. The shell's root is in the dispatch path for every action
            // in the window, so this is where a shortcut becomes an engine
            // command -- the same route a menu item, the command palette, the
            // CLI and a future agent take, so an edit made by a key and an edit
            // made by a click are the same edit.
            .on_action({
                let engine_canvas = self.engine_canvas.clone();
                let inspector = self.inspector.clone();
                move |action: &crate::engine::keymap::RunCommand, window, cx| {
                    let id = action.id.clone();
                    let params = action.params.clone();
                    // A TEXT SESSION IS ABOVE THE CANVAS.
                    //
                    // figcraft's own keymap says where each shortcut is live:
                    // the canvas context is "on the canvas (not while editing
                    // text)" and the text context is live only while editing
                    // text. The GPUI bindings Spool installs carry no context,
                    // so the shell enforces the engine's own rule here.
                    //
                    // Without it, typing a letter that is also a shortcut
                    // switches tools instead of typing: `L` is the line tool
                    // and `O` is the ellipse, `H` flips horizontally, and
                    // `Enter` descends the selection instead of ending a line.
                    //
                    // The key keeps travelling when the shortcut is skipped,
                    // because the character the user typed arrives as the
                    // platform's text input, and the key event is what tells
                    // the platform to send it.
                    if engine_canvas.read(cx).is_editing_text()
                        && !crate::engine::keymap::live_while_editing_text(&id)
                    {
                        cx.propagate();
                        return;
                    }
                    // A TEXT FIELD IS ABOVE THE CANVAS TOO.
                    //
                    // The engine's own text session is not the only place a
                    // character can be typed: every field in the Inspector is
                    // a text field, and the letters the engine's keymap binds
                    // as tool shortcuts -- `v`, `a`, `c`, `f`, `o`, `p`, `r`,
                    // `t` -- are letters in a colour code and in a name.
                    //
                    // GPUI dispatches a key binding BEFORE any `on_key_down`
                    // runs, and an action that is answered stops the key
                    // there: so a shortcut that ran while a field was open
                    // would switch the tool and the character would never
                    // reach the field. Skipping the shortcut lets the key
                    // travel on to the field that wants it.
                    if inspector.read(cx).has_text_field(window, cx)
                        && !crate::engine::keymap::live_while_editing_text(&id)
                    {
                        cx.propagate();
                        return;
                    }
                    // DEV MODE IS A MODE, AND THE ENGINE ONLY QUEUES A REQUEST
                    // FOR IT.
                    //
                    // `view.devMode` is figcraft's Shift+D, and the engine's
                    // command just asks for the panel to change -- it does not
                    // change it. So the shell is what answers, the way it
                    // answers the escape ladder: by switching the Inspector's
                    // tab, which is what the request meant.
                    if id == "view.devMode" {
                        inspector.update(cx, |inspector, cx| {
                            inspector.toggle_dev_mode(cx);
                        });
                        return;
                    }
                    // RENAME IS THE INSPECTOR'S, AND THE ENGINE ONLY ASKS.
                    //
                    // `object.rename` without a name is the engine asking the
                    // UI to rename a layer, which is what the Inspector's
                    // rename session is for. So the shell answers the request
                    // the way it answers Dev Mode's: by opening the rename,
                    // and the name the user types reaches the document through
                    // the engine's own `object.rename` when they commit it.
                    if id == "object.rename" && params.get("name").is_none() {
                        inspector.update(cx, |inspector, cx| {
                            inspector.open_rename_for_the_selection(cx);
                        });
                        return;
                    }
                    engine_canvas.update(cx, |canvas, cx| {
                        canvas.run_engine_command(&id, params, cx);
                    });
                }
            })
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.handle_key(
                    event.keystroke.key.as_str(),
                    event.keystroke.modifiers,
                    window,
                    cx,
                );
            }))
            .child(self.top_bar(cx))
            .child(self.render_body(cx));
        crate::diagnostics::record("shell_render_build", render_start);
        element
    }
}

pub(crate) fn agent_suggestion(icon: &'static str, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(10.0))
        .py(px(8.0))
        .rounded_md()
        .bg(rgb(theme::SURFACE_RAISED))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(div().text_color(rgb(theme::ACCENT)).child(icon))
        .child(label)
}

fn share_popover() -> impl IntoElement {
    div()
        .absolute()
        .top(px(40.0))
        .right(px(0.0))
        .flex()
        .flex_col()
        .gap_2()
        .w(px(240.0))
        .p(px(14.0))
        .rounded_lg()
        .bg(rgb(theme::SURFACE_RAISED))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .shadow_lg()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child("Share this project"),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child("Invites and shared links will be available here."),
        )
        .child(
            div()
                .mt(px(4.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded_md()
                .bg(rgb(theme::ACCENT_WASH))
                .text_xs()
                .text_color(rgb(theme::ACCENT))
                .child("Sharing not connected"),
        )
}

fn export_popover() -> impl IntoElement {
    div()
        .absolute()
        .top(px(40.0))
        .right(px(0.0))
        .flex()
        .flex_col()
        .gap_1()
        .w(px(205.0))
        .p(px(7.0))
        .rounded_lg()
        .bg(rgb(theme::SURFACE_RAISED))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .shadow_lg()
        .child(popover_row("Export selection…", "⇧ E"))
        .child(popover_row("Export page…", ""))
        .child(div().h(px(1.0)).my(px(4.0)).bg(rgb(theme::BORDER_SOFT)))
        .child(popover_row("Copy as SVG", ""))
        .child(popover_row("Copy properties", ""))
}

fn popover_row(label: &'static str, shortcut: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(9.0))
        .py(px(8.0))
        .rounded_md()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(label)
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child(shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    /// A launch that named nothing is the ordinary empty state. It is not an
    /// error, and it must not be reported as one.
    #[test]
    fn naming_no_project_is_not_an_error() {
        assert_eq!(project_request(None, None), None);
    }

    /// A `.spool` path on the command line is the product's own way in.
    #[test]
    fn a_command_line_path_is_a_project_request() {
        assert_eq!(
            project_request(Some(path("/tmp/MyProject.spool")), None),
            Some(ProjectRequest::Project(path("/tmp/MyProject.spool"))),
        );
    }

    /// `SPOOL_PROJECT` still works for development, and is labelled as such
    /// rather than being indistinguishable from the product path.
    #[test]
    fn the_development_override_is_a_distinct_request() {
        assert_eq!(
            project_request(None, Some(path("fixtures/landing"))),
            Some(ProjectRequest::DevelopmentOverride(path(
                "fixtures/landing"
            ))),
        );
    }

    /// The command line wins. A person who typed a path meant that path, and an
    /// inherited `SPOOL_PROJECT` must not silently redirect it.
    #[test]
    fn the_command_line_wins_over_the_development_override() {
        assert_eq!(
            project_request(
                Some(path("/tmp/MyProject.spool")),
                Some(path("fixtures/landing")),
            ),
            Some(ProjectRequest::Project(path("/tmp/MyProject.spool"))),
        );
    }

    /// The three states are genuinely distinct types, so no caller can handle an
    /// unusable project and a missing one the same way by accident.
    #[test]
    fn the_two_requests_are_not_interchangeable() {
        assert_ne!(
            ProjectRequest::Project(path("a.spool")),
            ProjectRequest::DevelopmentOverride(path("a.spool")),
        );
    }
}
