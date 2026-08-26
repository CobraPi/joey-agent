//! Typed wrappers for the CDP domains joey-browser uses (~8 domains).
//! Request/response structs + serde round-trip tests (T005).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `Target.createTarget` → returns targetId.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[allow(non_snake_case)]
pub struct CreateTargetResult {
    pub targetId: String,
}

/// `Target.attachToTarget` → sessionId.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[allow(non_snake_case)]
pub struct AttachToTargetResult {
    pub sessionId: String,
}

/// `Page.getFrameTree` node.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Origin if discoverable; None for opaque/about frames.
    #[serde(default)]
    pub origin: Option<String>,
    /// Security origin from CDP; merged into origin on parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_origin: Option<String>,
}

/// Nested frame tree entry.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrameTreeNode {
    pub frame: Frame,
    #[serde(default)]
    pub child_frames: Vec<FrameTreeNode>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[allow(non_snake_case)]
pub struct GetFrameTreeResult {
    pub frameTree: FrameTreeNode,
}

/// `Runtime.consoleAPICalled` event params (subset).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ConsoleEntry {
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub text: String,
}

/// `Page.javascriptDialogOpening` params (subset).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct DialogOpening {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub r#type: String,
}

/// `Page.captureScreenshot` result.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ScreenshotResult {
    pub data: String,
}

/// Input dispatch types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventType {
    Pressed,
    Released,
    Moved,
}

impl MouseEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MouseEventType::Pressed => "mousePressed",
            MouseEventType::Released => "mouseReleased",
            MouseEventType::Moved => "mouseMoved",
        }
    }
}

/// Builder for `Input.dispatchMouseEvent` params.
pub fn mouse_event(kind: MouseEventType, x: f64, y: f64, button: &str, clicks: i64) -> Value {
    json!({
        "type": kind.as_str(),
        "x": x,
        "y": y,
        "button": button,
        "clickCount": if kind == MouseEventType::Released { clicks } else if kind == MouseEventType::Pressed { clicks } else { 0 },
    })
}

/// Builder for `Input.dispatchKeyEvent` (rawKeyDown/char split handled by
/// caller per key).
pub fn key_event(kind: &str, key: &str, code: Option<&str>, modifiers: i64, text: Option<&str>) -> Value {
    let mut v = json!({
        "type": kind,
        "key": key,
        "modifiers": modifiers,
    });
    if let Some(c) = code {
        v["code"] = Value::String(c.to_string());
    }
    if let Some(t) = text {
        v["text"] = Value::String(t.to_string());
    }
    // Chrome ignores key events without a virtual key code for most
    // listeners (the DOM KeyboardEvent still fires, but synthetic text
    // input and some handlers need it).
    v["windowsVirtualKeyCode"] = Value::from(virtual_key_code(key));
    v["nativeVirtualKeyCode"] = Value::from(virtual_key_code(key));
    v
}

/// Best-effort US-layout virtual key codes (DOM `KeyboardEvent.keyCode`).
fn virtual_key_code(key: &str) -> u32 {
    match key {
        "Enter" => 13,
        "Tab" => 9,
        "Escape" => 27,
        "Backspace" => 8,
        "Delete" => 46,
        "Insert" => 45,
        "Home" => 36,
        "End" => 35,
        "PageUp" => 33,
        "PageDown" => 34,
        "ArrowUp" => 38,
        "ArrowDown" => 40,
        "ArrowLeft" => 37,
        "ArrowRight" => 39,
        " " => 32,
        "Shift" | "ShiftLeft" => 16,
        "Control" | "ControlLeft" => 17,
        "Alt" | "AltLeft" => 18,
        "Meta" | "MetaLeft" => 91,
        "F1" => 112, "F2" => 113, "F3" => 114, "F4" => 115, "F5" => 116,
        "F6" => 117, "F7" => 118, "F8" => 119, "F9" => 120, "F10" => 121,
        "F11" => 122, "F12" => 123,
        // Single printable ASCII: keyCode == char code (letters upper).
        k if k.len() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as u32
            } else {
                c as u32
            }
        }
        _ => 0,
    }
}

/// Modifier bitmask encoding — CDP `Input.dispatchKeyEvent` convention
/// (Puppeteer-verified): Alt=1, Ctrl=2, Meta=4, Shift=8.
pub fn modifier_bitmask(ctrl: bool, alt: bool, shift: bool, meta: bool) -> i64 {
    ((alt as i64) << 0) | ((ctrl as i64) << 1) | ((meta as i64) << 2) | ((shift as i64) << 3)
}

/// `Target.setAutoAttach` params with flatten.
pub fn set_auto_attach(auto_attach: bool, flatten: bool) -> Value {
    json!({ "autoAttach": auto_attach, "flatten": flatten, "waitForDebuggerOnStart": false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_target_round_trip() {
        let v: Value = serde_json::from_str(r#"{"targetId":"T1"}"#).unwrap();
        let r: CreateTargetResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.targetId, "T1");
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["targetId"], "T1");
    }

    #[test]
    fn attach_round_trip() {
        let r: AttachToTargetResult =
            serde_json::from_str(r#"{"sessionId":"S9"}"#).unwrap();
        assert_eq!(r.sessionId, "S9");
    }

    #[test]
    fn frame_tree_parses_camelcase() {
        let v: Value = serde_json::from_str(
            r#"{"frameTree":{"frame":{"id":"F1","parentId":null,"url":"https://a/"},"childFrames":[{"frame":{"id":"F2","url":"https://b/","name":"checkout"}}]}}"#,
        )
        .unwrap();
        let r: GetFrameTreeResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.frameTree.frame.id, "F1");
        assert_eq!(r.frameTree.child_frames.len(), 1);
        assert_eq!(r.frameTree.child_frames[0].frame.name.as_deref(), Some("checkout"));
    }

    #[test]
    fn dialog_and_console_parse() {
        let d: DialogOpening = serde_json::from_str(r#"{"message":"ok?","type":"confirm"}"#).unwrap();
        assert_eq!(d.r#type, "confirm");
        let c: ConsoleEntry = serde_json::from_str(r#"{"level":"error","text":"boom"}"#).unwrap();
        assert_eq!(c.text, "boom");
    }

    #[test]
    fn screenshot_round_trip() {
        let s: ScreenshotResult = serde_json::from_str(r#"{"data":"aGk="}"#).unwrap();
        assert_eq!(s.data, "aGk=");
    }

    #[test]
    fn input_builders() {
        let m = mouse_event(MouseEventType::Pressed, 10.5, 20.0, "left", 1);
        assert_eq!(m["type"], "mousePressed");
        assert_eq!(m["x"], 10.5);
        assert_eq!(m["clickCount"], 1);
        let moved = mouse_event(MouseEventType::Moved, 1.0, 1.0, "none", 2);
        assert_eq!(moved["clickCount"], 0, "moves carry no click count");

        let k = key_event("keyDown", "Enter", Some("Enter"), 2, Some("\r"));
        assert_eq!(k["modifiers"], 2);
        assert_eq!(k["code"], "Enter");
        assert_eq!(k["text"], "\r");
    }

    #[test]
    fn modifier_bitmask_convention() {
        assert_eq!(modifier_bitmask(false, false, false, false), 0);
        assert_eq!(modifier_bitmask(true, false, false, false), 2); // Ctrl=2
        assert_eq!(modifier_bitmask(false, true, false, false), 1); // Alt=1
        assert_eq!(modifier_bitmask(false, false, true, false), 8); // Shift=8
        assert_eq!(modifier_bitmask(false, false, false, true), 4); // Meta=4
    }

    #[test]
    fn auto_attach_params() {
        let v = set_auto_attach(true, true);
        assert_eq!(v["autoAttach"], true);
        assert_eq!(v["flatten"], true);
        assert_eq!(v["waitForDebuggerOnStart"], false);
    }
}
