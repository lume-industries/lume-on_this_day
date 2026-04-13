use lume_text_slide::{self as text_slide, Font, FontAssets, Lazy};
use text_slide::{Palette, TextAlign, TextBlock, Vertex, RuntimeOverlay, SlideSpec};

static FONT: Lazy<FontAssets> = Lazy::new(|| text_slide::make_font_assets(Font::VGA_8x14));

pub use on_this_day_sidecar::{EventRow, OnThisDayPayload};

static SPEC_BYTES: Lazy<Vec<u8>> =
    Lazy::new(|| text_slide::serialize_spec(&on_this_day_slide_spec()));

pub fn on_this_day_slide_spec() -> SlideSpec<Vertex> {
    text_slide::default_panel_spec("on_this_day_scene", build_overlay(None), palette(), FONT.atlas.clone())
}

pub fn serialized_spec() -> &'static [u8] {
    &SPEC_BYTES
}

fn build_overlay(payload: Option<&OnThisDayPayload>) -> RuntimeOverlay<Vertex> {
    if let Some(payload) = payload {
        let rows: Vec<String> = payload
            .events
            .iter()
            .map(|event| format!("{:>4}  {}", event.year, event.event))
            .collect();

        let mut blocks = vec![
            title_block("ON THIS DAY"),
            TextBlock {
                text: &payload.date_label,
                x: 160.0,
                y: 48.0,
                scale: 0.92,
                color: [0.74, 0.84, 0.94, 1.0],
                align: TextAlign::Center,
                wrap_cols: None,
            },
        ];
        for (idx, row) in rows.iter().enumerate() {
            blocks.push(TextBlock {
                text: row,
                x: 28.0,
                y: 72.0 + idx as f32 * 18.0,
                scale: 0.78,
                color: [1.0, 1.0, 1.0, 1.0],
                align: TextAlign::Left,
                wrap_cols: Some(34),
            });
        }
        blocks.push(footer_block(&payload.updated));
        return text_slide::compose_overlay(&blocks, &FONT);
    }

    text_slide::compose_overlay(&[
        title_block("ON THIS DAY"),
        TextBlock {
            text: "Loading Wikipedia history feed...",
            x: 160.0,
            y: 112.0,
            scale: 0.96,
            color: [1.0, 1.0, 1.0, 1.0],
            align: TextAlign::Center,
            wrap_cols: Some(24),
        },
    ], &FONT)
}

fn title_block(text: &'static str) -> TextBlock<'static> {
    TextBlock {
        text,
        x: 160.0,
        y: 26.0,
        scale: 1.04,
        color: [0.86, 0.94, 1.0, 1.0],
        align: TextAlign::Center,
        wrap_cols: None,
    }
}

fn footer_block<'a>(text: &'a str) -> TextBlock<'a> {
    TextBlock {
        text,
        x: 160.0,
        y: 198.0,
        scale: 0.78,
        color: [0.72, 0.82, 0.92, 1.0],
        align: TextAlign::Center,
        wrap_cols: None,
    }
}

fn palette() -> Palette {
    Palette {
        background: [0.02, 0.05, 0.08, 1.0],
        panel: [0.06, 0.10, 0.16, 0.96],
        accent: [0.18, 0.50, 0.80, 0.96],
        accent_soft: [0.08, 0.18, 0.28, 0.96],
    }
}


#[cfg(target_arch = "wasm32")]
lume_text_slide::VRX_64_slide::export_traced_entrypoints! {
    init = slide_init,
    update = slide_update,
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn vzglyd_spec_ptr() -> *const u8 {
    SPEC_BYTES.as_ptr()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn vzglyd_spec_len() -> u32 {
    SPEC_BYTES.len() as u32
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn vzglyd_abi_version() -> u32 {
    lume_text_slide::VRX_64_slide::ABI_VERSION
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
fn slide_init() -> i32 {
    runtime_state::state().refresh();
    0
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
fn slide_update(_dt: f32) -> i32 {
    let mut state = runtime_state::state();
    if let Some(payload) = text_slide::channel_runtime::poll_json::<OnThisDayPayload>(&mut state.response_buf) {
        state.payload = Some(payload);
    }
    state.refresh();
    1
}

#[cfg(target_arch = "wasm32")]
mod runtime_state {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::{OnThisDayPayload, build_overlay};
    use text_slide::channel_runtime;
    use crate::text_slide;

    pub struct RuntimeState {
        pub payload: Option<OnThisDayPayload>,
        pub overlay_bytes: Vec<u8>,
        pub response_buf: Vec<u8>,
    }

    impl RuntimeState {
        fn new() -> Self {
            let mut state = Self {
                payload: None,
                overlay_bytes: Vec::new(),
                response_buf: vec![0u8; text_slide::channel_runtime::CHANNEL_BUF_BYTES],
            };
            state.refresh();
            state
        }

        pub fn refresh(&mut self) {
            self.overlay_bytes =
                text_slide::serialize_overlay(&build_overlay(self.payload.as_ref()));
        }
    }

    static STATE: OnceLock<Mutex<RuntimeState>> = OnceLock::new();

    pub fn state() -> MutexGuard<'static, RuntimeState> {
        STATE
            .get_or_init(|| Mutex::new(RuntimeState::new()))
            .lock()
            .unwrap()
    }
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn vzglyd_overlay_ptr() -> *const u8 {
    runtime_state::state().overlay_bytes.as_ptr()
}

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn vzglyd_overlay_len() -> u32 {
    runtime_state::state().overlay_bytes.len() as u32
}

#[cfg(test)]
mod tests {
    use on_this_day_sidecar::{EventRow, OnThisDayPayload};
    use super::*;

    #[test]
    fn spec_valid() {
        on_this_day_slide_spec().validate().unwrap();
    }

    #[test]
    fn parse_events_sorts_and_truncates() {
        let body = r#"{
            "events": [
                {"year": 1800, "text": "An old event"},
                {"year": 2020, "text": "A very recent event with a description that should be shortened at a word boundary"},
                {"year": 1950, "text": "A middle event"}
            ]
        }"#;

        let payload =
            on_this_day_sidecar::parse_events_payload(body, "19 Mar".to_string(), 0).expect("parse events payload");
        assert_eq!(payload.events[0].year, "2020");
        assert!(payload.events[0].event.len() <= EVENT_MAX_LEN);
    }
}
