//! A getting-started window.
//!
//! The application's model is small but not guessable: an object does nothing
//! until a component is attached to it, and the domain is a scene node rather
//! than a menu. Both are the kind of thing a user only discovers by being told,
//! so this window tells them once, in the order they will need it.
//!
//! Deliberately static text rather than an interactive tour. A tour that drives
//! the panels would have to know their internals, and would rot the moment they
//! move; this reads like documentation because it is documentation.

use super::UiModel;

pub(super) const WINDOW_ID: &str = "help_window";

/// One walkthrough section: a heading and its ordered steps.
type Section = (&'static str, &'static [&'static str]);

const GETTING_STARTED: Section = (
    "Build a scene",
    &[
        "Scene panel → “+ Add object” places a bare object. On its own it has a \
         position and nothing else.",
        "With it selected, use the Inspector's Components → “+ Add” to give it \
         Charge, and it becomes a source of the electric field.",
        "Add Mass as well and it gains inertia, so the fields can push it. Tick \
         “Pinned” to hold it still and author its motion yourself.",
        "Add a Slice plane under Measurement to see the field across a surface, \
         or a Probe to record values at one point over time.",
        "Select the Simulation node to see the scene's fields. A field is \
         computed by one model at a time — the electric field can be solved \
         from static charges or advanced by Maxwell's equations, and it is the \
         same field either way.",
        "Press Play in the top bar. Step advances exactly one time step.",
    ],
);

const PANELS: Section = (
    "Where things live",
    &[
        "Scene (left) — everything the simulation contains. The Simulation node \
         at the top holds the domain, active field systems, and sampling.",
        "Inspector (right) — the properties of whatever is selected, and nothing \
         else.",
        "View (over the 3D view) — camera viewpoints and what is drawn, \
         including sparse arrows through the whole domain. These never change \
         the physics.",
        "Arrows are configured the same way everywhere they appear: how many, \
         and a scale factor on their length. Density decides how much of the \
         published field is drawn; the Simulation node's transport sampling is \
         what asks the solver for more of it.",
        "Top bar — Play, Pause, Step, Undo and Redo, the time step dt, and \
         playback speed.",
        "Both side panels are split into foldable sections. Collapse the ones \
         you are not working on; a folded section still shows how much is in it.",
    ],
);

const NAVIGATION: Section = (
    "Moving around",
    &[
        "Orbit — drag with the middle mouse button.",
        "Pan — hold Shift while dragging with the middle mouse button.",
        "Zoom — mouse wheel.",
        "Focus the selection — F, or the button in the View window.",
        "Standard viewpoints — 1, 3, and 7, or the axis buttons in View.",
        "Projection — Perspective or Orthographic, at the top of the View \
         window. Orthographic removes foreshortening, so lengths compare across \
         the whole view; the framing is unchanged either way.",
        "Move an object — select it, then drag an arrow for one axis, a corner \
         square for a plane, or the body itself to move it freely.",
    ],
);

const EDITING: Section = (
    "Editing a running simulation",
    &[
        "Dragging a body, or holding any value in the Inspector, pauses the run \
         for as long as you hold it — a drag teleports the object, and no \
         equation produced the poses in between. It resumes where it left off \
         when you let go.",
        "If the run was already paused, it stays paused. Editing never starts a \
         simulation.",
        "If a scene is too heavy to redraw as you drag, select the Simulation \
         node and clear “Update while editing” on the expensive field system. It \
         then recomputes once, when you release, from the values you committed.",
        "Undo and Redo in the top bar step through the scene edits you made — \
         Ctrl+Z and Ctrl+Shift+Z. A whole drag is one step, not one per frame.",
        "They need the simulation paused, and running it discards them: once a \
         solver has moved a body, the scene you edited is not the scene any \
         more. Undo edits the world, it does not rewind time.",
    ],
);

pub(super) fn help_window(context: &egui::Context, model: &mut UiModel) {
    let mut open = model.help_visible;
    egui::Window::new("Getting started")
        .id(egui::Id::new(WINDOW_ID))
        .open(&mut open)
        .default_pos(egui::pos2(330.0, 96.0))
        .default_width(400.0)
        .resizable(true)
        .collapsible(true)
        .show(context, |ui| {
            // Bounded and scrolled rather than as tall as its text. Opening on a
            // first run is only welcoming if the scene is still visible behind
            // it; a full-height window would read as a modal blocking the app.
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Field CAD models fields — a value at every point in space — and the \
                             objects that source them.",
                        )
                        .italics(),
                    )
                    .wrap(),
                );
                    for (heading, steps) in [GETTING_STARTED, PANELS, NAVIGATION, EDITING] {
                        ui.add_space(10.0);
                        ui.strong(heading);
                        ui.add_space(2.0);
                        for step in steps {
                            ui.horizontal_top(|ui| {
                                ui.label("•");
                                ui.add(egui::Label::new(*step).wrap());
                            });
                        }
                    }
                });
        });
    model.help_visible = open;
}
