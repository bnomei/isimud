//! macOS `isimud://` custom URL scheme handler.
//!
//! Receives the `GetURL` Apple Event via `NSAppleEventManager` when an `isimud://` link is
//! opened (requires a packaged `.app` with `CFBundleURLTypes`). Parsed `isimud://speak/...`
//! URLs become [`SpeakRequest`]s delivered into the tray event loop. Registration must happen
//! before the event loop runs to catch cold-launch URLs.

use isimud::voices::SpeakRequest;

/// Parse an `isimud://speak` URL into a [`SpeakRequest`].
///
/// Accepts the text in the path (`isimud://speak/Hello%20world`) or in a `text`
/// query parameter (`isimud://speak?text=Hello%20world`); optional `voice` and
/// `rate` query parameters are honored. Returns `None` for an unknown scheme or
/// verb, or when no non-empty text is supplied.
pub(crate) fn parse_speak_url(url: &str) -> Option<SpeakRequest> {
    let rest = strip_scheme(url.trim(), "isimud")?;
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let path = path.split('#').next().unwrap_or("");

    let mut segments = path.trim_start_matches('/').splitn(2, '/');
    let verb = segments.next().unwrap_or("").trim().to_ascii_lowercase();
    if verb != "speak" {
        return None;
    }
    let path_text = segments
        .next()
        .map(|segment| percent_decode(segment.trim()))
        .filter(|text| !text.is_empty());

    let (mut query_text, mut voice, mut rate) = (None, None, None);
    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = percent_decode(value);
            match key.to_ascii_lowercase().as_str() {
                "text" => query_text = Some(value),
                "voice" if !value.trim().is_empty() => voice = Some(value),
                "rate" => rate = value.trim().parse::<f32>().ok().filter(|r| r.is_finite()),
                _ => {}
            }
        }
    }

    let text = path_text.or(query_text)?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(SpeakRequest { text, voice, rate })
}

/// Strip a `scheme://` prefix (case-insensitive), returning the remainder.
fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let prefix = format!("{scheme}://");
    if url.len() < prefix.len() {
        return None;
    }
    let (head, tail) = url.split_at(prefix.len());
    head.eq_ignore_ascii_case(&prefix).then_some(tail)
}

/// Decode `%XX` percent-escapes, leaving any other bytes untouched.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::os::raw::c_char;
    use std::sync::OnceLock;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{class, define_class, msg_send, sel, AnyThread};
    use tao::event_loop::EventLoopProxy;
    use tracing::{info, warn};

    use super::parse_speak_url;
    use crate::runtime_tray::{send_user_event, UserEvent};
    use isimud::TARGET_RUNTIME;

    // FourCharCode constants from the Apple Event Manager headers.
    const KEY_DIRECT_OBJECT: u32 = 0x2D2D_2D2D; // '----'
    const INTERNET_EVENT_CLASS: u32 = 0x4755_524C; // 'GURL'
    const AE_GET_URL: u32 = 0x4755_524C; // 'GURL'

    static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

    define_class!(
        // SAFETY:
        // - The superclass NSObject has no subclassing requirements.
        // - `UrlSchemeHandler` does not implement `Drop` and has no ivars.
        #[unsafe(super(NSObject))]
        #[name = "IsimudUrlSchemeHandler"]
        struct UrlSchemeHandler;

        impl UrlSchemeHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(&self, event: *mut AnyObject, _reply: *mut AnyObject) {
                handle_get_url_event(event);
            }

            #[unsafe(method(applicationWillFinishLaunching:))]
            fn application_will_finish_launching(&self, _notification: *mut AnyObject) {
                register_get_url_handler(self);
            }
        }
    );

    fn handle_get_url_event(event: *mut AnyObject) {
        if event.is_null() {
            return;
        }
        let Some(url) = (unsafe { extract_url(event) }) else {
            warn!(target: TARGET_RUNTIME, "received isimud:// event without a URL string");
            return;
        };
        let Some(request) = parse_speak_url(&url) else {
            warn!(target: TARGET_RUNTIME, %url, "ignored unrecognized isimud:// URL");
            return;
        };
        let Some(proxy) = PROXY.get() else {
            warn!(target: TARGET_RUNTIME, "isimud:// handler invoked before proxy was installed");
            return;
        };
        send_user_event(proxy, UserEvent::Speak(request), "url_scheme_speak");
        info!(target: TARGET_RUNTIME, %url, "handled isimud:// speak URL");
    }

    unsafe fn extract_url(event: *mut AnyObject) -> Option<String> {
        let descriptor: *mut AnyObject =
            msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        if descriptor.is_null() {
            return None;
        }
        let string_value: *mut AnyObject = msg_send![descriptor, stringValue];
        if string_value.is_null() {
            return None;
        }
        let utf8: *const c_char = msg_send![string_value, UTF8String];
        if utf8.is_null() {
            return None;
        }
        CStr::from_ptr(utf8).to_str().ok().map(ToOwned::to_owned)
    }

    /// Install the `isimud://` `GetURL` Apple Event handler on the shared
    /// `NSAppleEventManager`.
    fn register_get_url_handler(handler: &UrlSchemeHandler) {
        let manager: *mut AnyObject =
            unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
        if manager.is_null() {
            warn!(target: TARGET_RUNTIME, "NSAppleEventManager unavailable; isimud:// disabled");
            return;
        }

        unsafe {
            let _: () = msg_send![
                manager,
                setEventHandler: handler,
                andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: INTERNET_EVENT_CLASS,
                andEventID: AE_GET_URL,
            ];
        }
        info!(target: TARGET_RUNTIME, "registered isimud:// URL scheme handler");
    }

    /// Register the `isimud://` URL scheme handler on the main thread.
    ///
    /// Must be called once, from the main thread, *before* the event loop runs.
    /// macOS dispatches the launch `GetURL` Apple Event during
    /// `applicationWillFinishLaunching` — earlier than tao's `StartCause::Init` —
    /// so we observe that notification to register the handler in time to catch
    /// the URL that cold-launched the app. We also register immediately to cover
    /// the already-running case.
    pub(crate) fn install_url_scheme_handler(proxy: EventLoopProxy<UserEvent>) {
        if PROXY.set(proxy).is_err() {
            warn!(target: TARGET_RUNTIME, "isimud:// URL handler already installed");
            return;
        }

        let handler: Retained<UrlSchemeHandler> =
            unsafe { msg_send![UrlSchemeHandler::alloc(), init] };

        unsafe {
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            if center.is_null() {
                warn!(target: TARGET_RUNTIME, "NSNotificationCenter unavailable; isimud:// cold-launch may be missed");
            } else {
                let name: *mut AnyObject = msg_send![
                    class!(NSString),
                    stringWithUTF8String: c"NSApplicationWillFinishLaunchingNotification".as_ptr()
                ];
                let _: () = msg_send![
                    center,
                    addObserver: &*handler,
                    selector: sel!(applicationWillFinishLaunching:),
                    name: name,
                    object: std::ptr::null::<AnyObject>(),
                ];
            }
        }

        register_get_url_handler(&handler);

        // The Apple Event Manager and notification center keep unretained
        // references to the handler, so it must outlive the process; intentionally
        // leak the single instance.
        std::mem::forget(handler);
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos::install_url_scheme_handler;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_from_path_in_authority_and_path_forms() {
        let request = parse_speak_url("isimud://speak/Hello%20world").unwrap();
        assert_eq!(request.text, "Hello world");
        assert_eq!(request.voice, None);
        assert_eq!(request.rate, None);

        assert_eq!(parse_speak_url("ISIMUD:///speak/Hi").unwrap().text, "Hi");
    }

    #[test]
    fn parses_text_voice_and_rate_from_query() {
        let request = parse_speak_url("isimud://speak?text=Hi%20there&voice=narrator&rate=1.25")
            .expect("query form should parse");
        assert_eq!(request.text, "Hi there");
        assert_eq!(request.voice.as_deref(), Some("narrator"));
        assert_eq!(request.rate, Some(1.25));
    }

    #[test]
    fn rejects_non_finite_rate() {
        for rate in ["nan", "inf", "-inf", "infinity", "NaN"] {
            let request = parse_speak_url(&format!("isimud://speak/hello?rate={rate}"))
                .expect("text is present so the request still parses");
            assert_eq!(request.rate, None, "rate={rate} should be rejected");
        }
    }

    #[test]
    fn path_text_takes_precedence_over_query_text() {
        let request = parse_speak_url("isimud://speak/from-path?text=from-query").unwrap();
        assert_eq!(request.text, "from-path");
    }

    #[test]
    fn rejects_unknown_scheme_verb_or_empty_text() {
        assert!(parse_speak_url("https://speak/Hello").is_none());
        assert!(parse_speak_url("isimud://stop").is_none());
        assert!(parse_speak_url("isimud://speak").is_none());
        assert!(parse_speak_url("isimud://speak/").is_none());
        assert!(parse_speak_url("isimud://speak?text=%20%20").is_none());
    }

    #[test]
    fn percent_decode_handles_escapes_and_passthrough() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("plain+text"), "plain+text");
        assert_eq!(percent_decode("trailing%"), "trailing%");
    }
}
