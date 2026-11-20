//! Click a light to bring the window of the process that sent its hooks to
//! the front.
//!
//! The hook server sees only a TCP connection from 127.0.0.1. On Windows the
//! connection table says which PID owns the client end, and the toolhelp
//! snapshot gives that PID's ancestors. The nearest ancestor with a real
//! top-level window (a terminal, VS Code) is the one to raise. PIDs and
//! process names live in memory only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use traffic_light_core::Session;

/// Failed lookups per session before we stop trying.
const MAX_TRIES: u8 = 5;
const MAX_ANCESTORS: usize = 12;

#[derive(Default)]
struct Origin {
    /// Sender first, then its parent, grandparent and so on.
    chain: Vec<(u32, String)>,
    tries: u8,
    in_flight: bool,
}

fn origins() -> &'static Mutex<HashMap<String, Origin>> {
    static ORIGINS: OnceLock<Mutex<HashMap<String, Origin>>> = OnceLock::new();
    ORIGINS.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Origin>> {
    origins().lock().unwrap_or_else(|e| e.into_inner())
}

/// Called for every event. Costs one map lookup once the origin is known.
pub fn remember(session_id: &str, peer: SocketAddr) {
    {
        let mut map = lock();
        let o = map.entry(session_id.to_owned()).or_default();
        if !o.chain.is_empty() || o.in_flight || o.tries >= MAX_TRIES {
            return;
        }
        o.tries += 1;
        o.in_flight = true;
    }
    // The hook's connection is still open while the server handles it, so the
    // table row exists right now; a spawned task could lose that race. The
    // slower ancestor walk can wait.
    #[cfg(debug_assertions)]
    let started = std::time::Instant::now();
    let pid = imp::owner_pid(peer);
    #[cfg(debug_assertions)]
    eprintln!(
        "traffic-light: session {session_id} pid lookup {pid:?} took {:?}",
        started.elapsed()
    );
    let id = session_id.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let chain = pid
            .map(|p| imp::ancestry(p, MAX_ANCESTORS))
            .unwrap_or_default();
        #[cfg(debug_assertions)]
        eprintln!("traffic-light: session {id} chain: {}", describe(&chain));
        let mut map = lock();
        if let Some(o) = map.get_mut(&id) {
            o.in_flight = false;
            o.chain = chain;
        }
    });
}

/// Forget sessions that are gone.
pub fn retain(sessions: &[Session]) {
    lock().retain(|id, _| sessions.iter().any(|s| &s.session_id == id));
}

/// Raise the window that owns this session. Returns whether one was found.
/// `label` is the session's folder name: one process often owns several
/// windows (every VS Code window belongs to the same Code.exe), and the one
/// whose title names the folder is the right one.
/// `own` is the widget's window; focusing it first gives us the right to
/// take the foreground, which a `"focus": false` window otherwise lacks.
pub fn focus(session_id: &str, label: &str, own: Option<&tauri::WebviewWindow>) -> bool {
    let chain = match lock().get(session_id) {
        Some(o) => o.chain.clone(),
        None => return false,
    };
    if chain.is_empty() {
        return false;
    }
    if let Some(w) = own {
        let _ = w.set_focus();
    }
    imp::raise(&chain, label)
}

#[cfg(debug_assertions)]
fn describe(chain: &[(u32, String)]) -> String {
    chain
        .iter()
        .map(|(p, n)| format!("{p} {n}"))
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Debug aid: `TL_FOCUS_TEST=<session_id>` focuses that session once its
/// origin is known, without the UI, and logs what happened.
#[cfg(debug_assertions)]
pub fn debug_test(app: tauri::AppHandle) {
    use tauri::Manager;
    let Ok(id) = std::env::var("TL_FOCUS_TEST") else {
        return;
    };
    std::thread::spawn(move || {
        for _ in 0..120 {
            let known = lock().get(&id).is_some_and(|o| !o.chain.is_empty());
            if known {
                // set_focus is queued to the main thread; a real click runs
                // on it directly, so give the queue time to drain.
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_focus();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                let ok = focus(&id, "", None);
                eprintln!("traffic-light: TL_FOCUS_TEST {id} -> {ok}");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        eprintln!("traffic-light: TL_FOCUS_TEST {id}: no origin recorded within 60 s");
    });
}

#[cfg(windows)]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::net::{IpAddr, SocketAddr};

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_CONNECTIONS,
    };
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindow, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow,
        ShowWindow, GW_OWNER, SW_RESTORE,
    };

    /// PID that owns the client end of a loopback connection: the row whose
    /// *local* address is the peer address. (The server's own row has the
    /// peer as its remote address.)
    pub fn owner_pid(peer: SocketAddr) -> Option<u32> {
        let IpAddr::V4(ip) = peer.ip() else {
            return None;
        };
        let want_addr = u32::from_ne_bytes(ip.octets());
        // Ports are stored in network byte order in the low 16 bits.
        let want_port = peer.port().to_be() as u32;

        // The table can grow between the size query and the read.
        let mut buf: Vec<u8> = vec![0; 16 * 1024];
        for _ in 0..4 {
            let mut size = buf.len() as u32;
            let rc = unsafe {
                GetExtendedTcpTable(
                    Some(buf.as_mut_ptr().cast()),
                    &mut size,
                    false,
                    AF_INET.0 as u32,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                )
            };
            match rc {
                0 => {
                    let count = u32::from_ne_bytes(buf[..4].try_into().ok()?) as usize;
                    let rows = unsafe {
                        std::slice::from_raw_parts(
                            buf.as_ptr().add(4).cast::<MIB_TCPROW_OWNER_PID>(),
                            count,
                        )
                    };
                    return rows
                        .iter()
                        .find(|r| r.dwLocalAddr == want_addr && r.dwLocalPort & 0xffff == want_port)
                        .map(|r| r.dwOwningPid);
                }
                122 => buf.resize(size as usize + 4096, 0), // ERROR_INSUFFICIENT_BUFFER
                _ => return None,
            }
        }
        None
    }

    fn exe_name(e: &PROCESSENTRY32W) -> String {
        let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&e.szExeFile[..len])
    }

    /// `pid` and up to `max` ancestors. Stops at the system processes, at a
    /// parent that no longer exists, and on a cycle (PIDs get reused).
    pub fn ancestry(pid: u32, max: usize) -> Vec<(u32, String)> {
        let mut procs: HashMap<u32, (u32, String)> = HashMap::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return Vec::new();
            };
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                procs.insert(e.th32ProcessID, (e.th32ParentProcessID, exe_name(&e)));
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = pid;
        while chain.len() <= max && cur != 0 && cur != 4 && seen.insert(cur) {
            let Some((parent, name)) = procs.get(&cur) else {
                break;
            };
            chain.push((cur, name.clone()));
            cur = *parent;
        }
        chain
    }

    struct Search {
        pid: u32,
        /// Candidate windows of `pid`, front to back.
        found: Vec<(HWND, String)>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        let mut owner_pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut owner_pid));
        if owner_pid != search.pid
            || !IsWindowVisible(hwnd).as_bool()
            || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid())
        {
            return true.into();
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return true.into();
        }
        let mut text = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        search
            .found
            .push((hwnd, String::from_utf16_lossy(&text[..n])));
        true.into()
    }

    /// The window of `pid` whose title mentions `label`, else its frontmost.
    fn window_of(pid: u32, label: &str) -> Option<(HWND, String)> {
        let mut search = Search {
            pid,
            found: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        let label = label.to_lowercase();
        let pick = (!label.is_empty())
            .then(|| {
                search
                    .found
                    .iter()
                    .position(|(_, t)| t.to_lowercase().contains(&label))
            })
            .flatten()
            .unwrap_or(0);
        (pick < search.found.len()).then(|| search.found.swap_remove(pick))
    }

    pub fn raise(chain: &[(u32, String)], label: &str) -> bool {
        let own = unsafe { GetCurrentProcessId() };
        for (pid, name) in chain {
            if *pid == own || name.eq_ignore_ascii_case("explorer.exe") {
                continue;
            }
            let Some((hwnd, title)) = window_of(*pid, label) else {
                continue;
            };
            #[cfg(debug_assertions)]
            eprintln!("traffic-light: focusing {pid} {name}, window {title:?}");
            #[cfg(not(debug_assertions))]
            let _ = title;
            bring_to_front(hwnd);
            return true;
        }
        false
    }

    fn bring_to_front(hwnd: HWND) {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
            if GetForegroundWindow() == hwnd {
                #[cfg(debug_assertions)]
                eprintln!("traffic-light: SetForegroundWindow worked directly");
                return;
            }
            // Windows refused. Borrow the current foreground thread's input
            // queue, which lets this thread take the foreground.
            let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0
                && fg_thread != me
                && AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
            #[cfg(debug_assertions)]
            eprintln!(
                "traffic-light: used AttachThreadInput fallback, foreground now matches: {}",
                GetForegroundWindow() == hwnd
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::net::SocketAddr;

    pub fn owner_pid(_: SocketAddr) -> Option<u32> {
        None
    }
    pub fn ancestry(_: u32, _: usize) -> Vec<(u32, String)> {
        Vec::new()
    }
    pub fn raise(_: &[(u32, String)], _: &str) -> bool {
        false
    }
}
