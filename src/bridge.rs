//! The seam between the pipeline subsystem and the rwire UI.
//!
//! The pipeline runs off the async-std UI thread and pushes read-model updates into
//! shared `App` state via `update_shared_changed`, dirtying only the fields it
//! touched so unrelated renderers are skipped (the llama-monitor projector pattern).
//!
//! Phase 1 wires the background→UI direction with a stand-in for pipeline boot: it
//! flips `pipeline_ready` once the subsystem is up. The real pipeline (and the
//! UI→pipeline command channel) grow in later phases.

use std::sync::Arc;
use std::time::Duration;

use rwire::{ChangeSet, SharedServerState};

use crate::state::App;

pub fn spawn(shared: Arc<SharedServerState>) {
    std::thread::spawn(move || run(&shared));
}

fn run(shared: &SharedServerState) {
    // Stand-in for pipeline startup: clients paint "starting…" first, then this
    // flips to "ready" live — exercising the projector end to end.
    std::thread::sleep(Duration::from_millis(400));
    mark_ready(shared);
}

fn mark_ready(shared: &SharedServerState) {
    shared.update_shared_changed::<App>(
        ChangeSet::from_fields(&[App::FIELD_PIPELINE_READY]),
        |app| {
            app.pipeline_ready = true;
        },
    );
}
