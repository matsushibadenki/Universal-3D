//! Layout messages contain CSS bounds only. Scene and frame data never cross IPC.
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
pub struct ViewportRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
    #[serde(rename = "viewportHeight")]
    pub viewport_height: f64,
    pub visible: bool,
}
#[derive(Debug, PartialEq)]
struct Layout {
    full: [f64; 4],
    clip: [f64; 4],
    pixels: [u32; 2],
}
impl ViewportRect {
    fn layout(self, bounds: [f64; 2], scale: f64) -> Result<Option<Layout>, String> {
        if ![
            self.x,
            self.y,
            self.width,
            self.height,
            self.dpr,
            self.viewport_height,
            scale,
            bounds[0],
            bounds[1],
        ]
        .iter()
        .all(|v| v.is_finite())
            || !(0.1..=8.).contains(&self.dpr)
            || self.viewport_height <= 0.
            || scale <= 0.
            || self.width < 0.
            || self.height < 0.
        {
            return Err("Invalid viewport bounds".into());
        }
        if !self.visible || self.width < 1. || self.height < 1. {
            return Ok(None);
        }
        let factor = self.dpr / scale; // CSS zoom and display DPI are independent.
        let full = [
            self.x * factor,
            self.y * factor,
            self.width * factor,
            self.height * factor,
        ];
        let left = full[0].max(0.);
        let top = full[1].max(0.);
        let right = (full[0] + full[2]).min(bounds[0]);
        let bottom = (full[1] + full[3]).min(bounds[1]);
        if right <= left || bottom <= top {
            return Ok(None);
        }
        let pixels = [
            (self.width * self.dpr).round(),
            (self.height * self.dpr).round(),
        ];
        // Avoid arbitrary allocations from malformed IPC. Compatible with default wgpu limits.
        if pixels.iter().any(|v| !(1. ..=8192.).contains(v)) {
            return Err("Viewport exceeds GPU size limit".into());
        }
        Ok(Some(Layout {
            full,
            clip: [left, top, right - left, bottom - top],
            pixels: [pixels[0] as u32, pixels[1] as u32],
        }))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::{renderer::Renderer, State};
    use objc2::{rc::Retained, MainThreadMarker};
    use objc2_app_kit::{NSColor, NSView, NSWindow, NSWindowOrderingMode};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use objc2_quartz_core::{CAMetalLayer, CATransaction};
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    struct NativeViewport {
        clip: Retained<NSView>,
        view: Retained<NSView>,
        layer: Retained<CAMetalLayer>,
    }
    thread_local! { static HOST: RefCell<Option<NativeViewport>> = const { RefCell::new(None) }; }

    pub fn update(
        web_ptr: *mut std::ffi::c_void,
        window_ptr: *mut std::ffi::c_void,
        rect: ViewportRect,
        scale: f64,
        state: &State,
    ) -> Result<bool, String> {
        let mtm = MainThreadMarker::new().ok_or("Viewport layout must run on the main thread")?;
        // Tauri supplies these handles in with_webview on the owning native event loop.
        let web = unsafe { &*web_ptr.cast::<NSView>() };
        let window = unsafe { &*window_ptr.cast::<NSWindow>() };
        let bounds = web.bounds();
        // WKWebView can extend under the titlebar while its CSS viewport excludes that inset.
        let css_height = (rect.viewport_height * rect.dpr / scale).min(bounds.size.height);
        let top_inset = (bounds.size.height - css_height).max(0.);
        let layout = rect.layout([bounds.size.width, css_height], scale)?;
        HOST.with_borrow_mut(|host| {
            let Some(layout) = layout else {
                if let Some(host) = host {
                    host.clip.setHidden(true);
                }
                state.viewport_visible.store(false, Ordering::Release);
                return Ok(false);
            };
            let parent = unsafe { web.superview() }.ok_or("Missing WebView parent")?;
            let created = host.is_none();
            if created {
                let clip = NSView::new(mtm);
                clip.setWantsLayer(true);
                clip.layer()
                    .ok_or("Missing clipping layer")?
                    .setMasksToBounds(true);
                let view = NSView::new(mtm);
                let layer = CAMetalLayer::new();
                view.setLayer(Some(&layer));
                view.setWantsLayer(true);
                clip.addSubview(&view);
                // Keep the window opaque; only the WKWebView backing is transparent.
                window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    0.14, 0.14, 0.14, 1.,
                )));
                window.setOpaque(true);
                parent.addSubview_positioned_relativeTo(
                    &clip,
                    NSWindowOrderingMode::Below,
                    Some(web),
                );
                let renderer = unsafe {
                    pollster::block_on(Renderer::from_metal_layer(
                        Retained::as_ptr(&layer).cast_mut().cast(),
                        layout.pixels[0],
                        layout.pixels[1],
                    ))
                };
                match renderer {
                    Ok(renderer) => {
                        *state.renderer.lock().map_err(|e| e.to_string())? = Some(renderer)
                    }
                    Err(error) => {
                        clip.removeFromSuperview();
                        return Err(error);
                    }
                }
                *host = Some(NativeViewport { clip, view, layer });
            }
            let host = host.as_ref().unwrap();
            let to_parent = |r: [f64; 4]| {
                let y = if web.isFlipped() {
                    r[1] + top_inset
                } else {
                    bounds.size.height - top_inset - r[1] - r[3]
                };
                web.convertRect_toView(
                    NSRect::new(NSPoint::new(r[0], y), NSSize::new(r[2], r[3])),
                    Some(&parent),
                )
            };
            let full = to_parent(layout.full);
            let clip = to_parent(layout.clip);
            // Disable Core Animation interpolation while CSS layout is moving/resizing.
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            host.clip.setFrame(clip);
            host.view.setFrame(NSRect::new(
                NSPoint::new(full.origin.x - clip.origin.x, full.origin.y - clip.origin.y),
                full.size,
            ));
            host.layer.setContentsScale(scale);
            host.clip.setHidden(false);
            CATransaction::commit();
            if let Some(renderer) = state.renderer.lock().map_err(|e| e.to_string())?.as_mut() {
                renderer.resize(layout.pixels[0], layout.pixels[1]);
            }
            state.viewport_visible.store(true, Ordering::Release);
            Ok(created)
        })
    }
    pub fn destroy() {
        HOST.with_borrow_mut(|host| {
            if let Some(host) = host.take() {
                host.clip.removeFromSuperview();
            }
        });
    }
}
#[cfg(target_os = "macos")]
pub use macos::{destroy, update};

#[cfg(test)]
mod tests {
    use super::*;
    fn rect() -> ViewportRect {
        ViewportRect {
            x: 200.,
            y: 100.,
            width: 600.,
            height: 400.,
            dpr: 2.,
            viewport_height: 800.,
            visible: true,
        }
    }
    #[test]
    fn dpi_zoom_and_clipping_preserve_full_projection_size() {
        let layout = rect().layout([1000., 800.], 2.).unwrap().unwrap();
        assert_eq!(layout.full, [200., 100., 600., 400.]);
        assert_eq!(layout.pixels, [1200, 800]);
        let zoomed = ViewportRect { dpr: 3., ..rect() }
            .layout([1000., 800.], 2.)
            .unwrap()
            .unwrap();
        assert_eq!(zoomed.full, [300., 150., 900., 600.]);
        assert_eq!(zoomed.clip, [300., 150., 700., 600.]);
        assert_eq!(zoomed.pixels, [1800, 1200]);
        let scrolled = ViewportRect { y: -100., ..rect() }
            .layout([1000., 800.], 2.)
            .unwrap()
            .unwrap();
        assert_eq!(scrolled.clip, [200., 0., 600., 300.]);
        assert_eq!(scrolled.pixels, [1200, 800]);
    }
    #[test]
    fn hidden_zero_offscreen_and_invalid_layouts() {
        assert!(ViewportRect {
            visible: false,
            ..rect()
        }
        .layout([1000., 800.], 2.)
        .unwrap()
        .is_none());
        assert!(ViewportRect {
            width: 0.,
            ..rect()
        }
        .layout([1000., 800.], 2.)
        .unwrap()
        .is_none());
        assert!(ViewportRect { y: 900., ..rect() }
            .layout([1000., 800.], 2.)
            .unwrap()
            .is_none());
        assert!(ViewportRect {
            dpr: f64::NAN,
            ..rect()
        }
        .layout([1000., 800.], 2.)
        .is_err());
        assert!(ViewportRect {
            width: 10000.,
            ..rect()
        }
        .layout([1000., 800.], 2.)
        .is_err());
    }
}
