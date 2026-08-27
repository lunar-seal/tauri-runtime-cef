use std::sync::{Arc, Mutex};

use cef::{DraggableRegion, ImplWindow, Window};
use tauri_runtime::webview::InitializationScript;

use crate::cef_impl::client::DraggableRegionsChanged;

const DRAG_REGION_SCRIPT: &str = r#"
(() => {
  addEventListener("DOMContentLoaded", () => {
    const style = document.createElement("style");
    const nonce = document.querySelector("style[nonce], script[nonce]")?.nonce;
    if (nonce) style.nonce = nonce;
    style.textContent = `
      [data-tauri-drag-region]:not([data-tauri-drag-region="false"]) {
        -webkit-app-region: drag;
      }
      [data-tauri-drag-region]:not([data-tauri-drag-region="deep"]) * {
        -webkit-app-region: no-drag;
      }
      [data-tauri-drag-region="false"],
      [data-tauri-drag-region="deep"] :is(a, button, input, select, textarea, label, summary,
        [contenteditable]:not([contenteditable="false"]), [tabindex]:not([tabindex="-1"]),
        [role="button"], [role="link"], [role="menuitem"], [role="tab"], [role="checkbox"],
        [role="radio"], [role="switch"], [role="option"]):not([data-tauri-drag-region]) {
        -webkit-app-region: no-drag;
      }
    `;
    (document.head || document.documentElement).append(style);
  }, { once: true });
})();
"#;

pub(crate) fn drag_region_initialization_script() -> InitializationScript {
  InitializationScript {
    script: DRAG_REGION_SCRIPT.to_string(),
    for_main_frame_only: true,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn drag_region_style_waits_for_and_reuses_the_csp_nonce() {
    let script = drag_region_initialization_script();
    assert!(script.for_main_frame_only);
    assert!(script.script.contains("DOMContentLoaded"));
    assert!(script.script.contains("style.nonce = nonce"));
  }
}

#[derive(Default)]
struct State {
  window: Option<Window>,
  regions: Vec<DraggableRegion>,
}

#[derive(Clone, Default)]
pub(super) struct DraggableRegions(Arc<Mutex<State>>);

impl DraggableRegions {
  pub(super) fn attach(&self, window: Window) {
    let regions = {
      let mut state = self.0.lock().unwrap();
      state.window = Some(window.clone());
      state.regions.clone()
    };
    window.set_draggable_regions((!regions.is_empty()).then_some(regions.as_slice()));
  }

  fn set(&self, regions: Option<&[DraggableRegion]>) {
    let (window, regions) = {
      let mut state = self.0.lock().unwrap();
      state.regions = regions.unwrap_or_default().to_vec();
      (state.window.clone(), state.regions.clone())
    };
    if let Some(window) = window {
      window.set_draggable_regions((!regions.is_empty()).then_some(regions.as_slice()));
    }
  }

  pub(super) fn changed_handler(&self) -> DraggableRegionsChanged {
    let regions = self.clone();
    Arc::new(move |changed| regions.set(changed))
  }
}
