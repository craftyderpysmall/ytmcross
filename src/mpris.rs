//! MPRIS: how the desktop sees and controls a media player (media keys,
//! bars, `playerctl`). One thread, asleep in poll until the bus or the app
//! has something for it; it starts with the first playback.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::LocalConnection;
use dbus::channel::{BusType, Channel, MatchingReceiver, Sender};
use dbus::message::MatchRule;

use crate::api::models::Track;
use crate::config::Control;

const NAME: &str = "org.mpris.MediaPlayer2.ytmcross";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";

const INTROSPECTION: &str = r#"<node>
<interface name="org.mpris.MediaPlayer2">
<method name="Raise"/><method name="Quit"/>
<property name="CanQuit" type="b" access="read"/><property name="CanRaise" type="b" access="read"/>
<property name="HasTrackList" type="b" access="read"/><property name="Identity" type="s" access="read"/>
<property name="DesktopEntry" type="s" access="read"/><property name="SupportedUriSchemes" type="as" access="read"/>
<property name="SupportedMimeTypes" type="as" access="read"/>
</interface>
<interface name="org.mpris.MediaPlayer2.Player">
<method name="Next"/><method name="Previous"/><method name="Pause"/><method name="PlayPause"/>
<method name="Stop"/><method name="Play"/>
<property name="PlaybackStatus" type="s" access="read"/><property name="Rate" type="d" access="read"/>
<property name="Metadata" type="a{sv}" access="read"/><property name="Volume" type="d" access="read"/>
<property name="Position" type="x" access="read"/><property name="MinimumRate" type="d" access="read"/>
<property name="MaximumRate" type="d" access="read"/><property name="CanGoNext" type="b" access="read"/>
<property name="CanGoPrevious" type="b" access="read"/><property name="CanPlay" type="b" access="read"/>
<property name="CanPause" type="b" access="read"/><property name="CanSeek" type="b" access="read"/>
<property name="CanControl" type="b" access="read"/>
</interface>
</node>"#;

/// What the bus can ask about.
#[derive(Clone, Default, PartialEq)]
struct Info {
    track: Option<Track>,
    paused: bool,
    volume: f64,
    seconds: f64,
}

struct State {
    info: Info,
    /// Position when last told, and when that was.
    position: f64,
    at: Instant,
}

/// The app's end: `update` is cheap enough to call on every wake-up.
pub struct Mpris {
    state: Arc<Mutex<State>>,
    /// A byte here wakes the thread: the state changed.
    wake: UnixStream,
}

impl Mpris {
    pub fn start(control: impl Fn(Control) + Send + 'static) -> std::io::Result<Mpris> {
        let (wake, wake_rx) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        let state = Arc::new(Mutex::new(State { info: Info::default(), position: 0.0, at: Instant::now() }));
        let shared = state.clone();
        std::thread::Builder::new().name("mpris".into()).spawn(move || {
            // No session bus: nothing to do, and the app works the same.
            if let Err(e) = serve(shared, wake_rx, control) {
                crate::trace!("mpris: {e}");
            }
        })?;
        Ok(Mpris { state, wake })
    }

    pub fn update(&mut self, track: Option<&Track>, paused: bool, position: f64, seconds: f64, volume: f32) {
        let volume = f64::from(volume);
        let mut s = self.state.lock().unwrap();
        s.position = position;
        s.at = Instant::now();
        let i = &s.info;
        if i.track.as_ref() != track || i.paused != paused || i.seconds != seconds || i.volume != volume {
            s.info = Info { track: track.cloned(), paused, volume, seconds };
            drop(s);
            // A full socket has a wake-up in it already.
            let _ = self.wake.write(&[1]);
        }
    }
}

fn serve(state: Arc<Mutex<State>>, mut wake: UnixStream, control: impl Fn(Control) + 'static) -> Result<(), dbus::Error> {
    let mut channel = Channel::get_private(BusType::Session)?;
    // Without this the file descriptor below can't be asked for.
    channel.set_watch_enabled(true);
    let bus = LocalConnection::from(channel);
    // Another instance has the name: take a name of our own.
    if bus.request_name(NAME, false, false, true).is_err() {
        bus.request_name(format!("{NAME}.instance{}", std::process::id()), false, false, true)?;
    }
    let handler = state.clone();
    bus.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, bus| {
            if let Some(reply) = answer(&msg, &handler, &control) {
                let _ = bus.send(reply);
            }
            true
        }),
    );
    let mut shown = Info::default();
    let bus_fd = bus.channel().watch().fd;
    loop {
        let mut fds = [bus_fd, wake.as_raw_fd()].map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
        // SAFETY: the array outlives the call, and its length is passed along.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            continue;
        }
        if fds[1].revents != 0 {
            let mut bytes = [0; 64];
            if matches!(wake.read(&mut bytes), Ok(0) | Err(_)) {
                return Ok(()); // the app is gone
            }
            let info = state.lock().unwrap().info.clone();
            if info != shown {
                let _ = bus.send(changed(&info));
                shown = info;
            }
        }
        while bus.process(Duration::ZERO)? {}
        bus.channel().flush();
    }
}

/// The reply to a method call, if it needs one.
fn answer(msg: &Message, state: &Mutex<State>, control: &impl Fn(Control)) -> Option<Message> {
    if msg.get_no_reply() && msg.interface().as_deref() != Some(PLAYER) {
        return None;
    }
    let interface = msg.interface();
    let member = msg.member();
    let reply = msg.method_return();
    let paused = || state.lock().unwrap().info.paused;
    Some(match (interface.as_deref(), member.as_deref()) {
        (Some(PLAYER), Some(method)) => {
            match method {
                "Next" => control(Control::Next),
                "Previous" => control(Control::Previous),
                "PlayPause" => control(Control::PlayPause),
                "Play" if paused() => control(Control::PlayPause),
                "Pause" | "Stop" if !paused() => control(Control::PlayPause),
                _ => {}
            }
            reply
        }
        (Some(ROOT), Some(_)) => reply,
        (Some(PROPERTIES), Some("GetAll")) => reply.append1(properties(msg.get1::<&str>()?, state)),
        (Some(PROPERTIES), Some("Get")) => {
            let (interface, name) = msg.get2::<&str, &str>();
            match properties(interface?, state).remove(name?) {
                Some(value) => reply.append1(value),
                None => msg.error(&"org.freedesktop.DBus.Error.UnknownProperty".into(), c"no such property"),
            }
        }
        (Some(INTROSPECTABLE), Some("Introspect")) => reply.append1(INTROSPECTION),
        _ => msg.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"no such method"),
    })
}

fn value(v: impl RefArg + 'static) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(v))
}

fn properties(interface: &str, state: &Mutex<State>) -> PropMap {
    let mut map = PropMap::new();
    let mut set = |name: &str, v: Variant<Box<dyn RefArg>>| map.insert(name.to_string(), v);
    match interface {
        ROOT => {
            set("CanQuit", value(false));
            set("CanRaise", value(false));
            set("HasTrackList", value(false));
            set("Identity", value("YouTube Music".to_string()));
            set("DesktopEntry", value("ytmcross".to_string()));
            set("SupportedUriSchemes", value(Vec::<String>::new()));
            set("SupportedMimeTypes", value(Vec::<String>::new()));
        }
        PLAYER => {
            let s = state.lock().unwrap();
            let position = s.position + if s.info.paused { 0.0 } else { s.at.elapsed().as_secs_f64() };
            for (name, v) in player_changes(&s.info) {
                set(&name, v);
            }
            set("Position", value((position * 1e6) as i64));
            set("Rate", value(1.0));
            set("MinimumRate", value(1.0));
            set("MaximumRate", value(1.0));
            set("CanGoNext", value(true));
            set("CanGoPrevious", value(true));
            set("CanPlay", value(true));
            set("CanPause", value(true));
            set("CanSeek", value(false));
            set("CanControl", value(true));
        }
        _ => {}
    }
    map
}

/// The properties `changed` announces.
fn player_changes(info: &Info) -> PropMap {
    let status = match (&info.track, info.paused) {
        (None, _) => "Stopped",
        (_, true) => "Paused",
        (_, false) => "Playing",
    };
    let mut map = PropMap::new();
    map.insert("PlaybackStatus".to_string(), value(status.to_string()));
    map.insert("Metadata".to_string(), value(metadata(info)));
    map.insert("Volume".to_string(), value(info.volume));
    map
}

fn metadata(info: &Info) -> PropMap {
    let mut map: PropMap = HashMap::new();
    let Some(t) = &info.track else {
        map.insert("mpris:trackid".to_string(), value(dbus::Path::from("/org/mpris/MediaPlayer2/TrackList/NoTrack")));
        return map;
    };
    // Object paths allow fewer characters than video ids have.
    let id: String = t.video_id.bytes().map(|b| format!("{b:02x}")).collect();
    let path = dbus::Path::from(format!("/org/mpris/MediaPlayer2/ytmcross/track/{id}"));
    map.insert("mpris:trackid".to_string(), value(path));
    map.insert("mpris:length".to_string(), value((info.seconds * 1e6) as i64));
    map.insert("xesam:title".to_string(), value(t.title.clone()));
    map.insert("xesam:artist".to_string(), value(vec![t.artists.clone()]));
    map.insert("xesam:url".to_string(), value(format!("https://music.youtube.com/watch?v={}", t.video_id)));
    if let Some(album) = &t.album {
        map.insert("xesam:album".to_string(), value(album.clone()));
    }
    if let Some(art) = &t.thumb {
        map.insert("mpris:artUrl".to_string(), value(art.clone()));
    }
    map
}

fn changed(info: &Info) -> Message {
    let invalidated: Vec<String> = Vec::new();
    Message::signal(&PATH.into(), &PROPERTIES.into(), &"PropertiesChanged".into()).append3(PLAYER, player_changes(info), invalidated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            video_id: "IFcAe8-Ep_0".into(),
            title: "So What".into(),
            artists: "Miles Davis".into(),
            album: Some("Kind of Blue".into()),
            duration: Some("9:22".into()),
            thumb: Some("https://example.com/a.jpg".into()),
            set_video_id: None,
            artist_links: Vec::new(),
            album_id: None,
            liked: None,
            plays: None,
        }
    }

    #[test]
    fn builds_every_message() {
        for t in [Some(track()), None] {
            let info = Info { track: t, paused: false, volume: 0.8, seconds: 562.0 };
            let _ = changed(&info);
            let state = Mutex::new(State { info, position: 1.5, at: Instant::now() });
            assert_eq!(properties(ROOT, &state).len(), 7);
            assert_eq!(properties(PLAYER, &state).len(), 13);
        }
    }
}
