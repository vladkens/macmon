//! Open files and sockets of a process, read through libproc as `lsof` reads them.
//!
//! The structs libc lacks are written by hand from the macOS SDK `sys/proc_info.h`, with only the
//! fields that are read; padding arrays keep the rest of the layout.

use std::ffi::{c_char, c_int, c_void};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::{io, mem};

use crate::procs::c_chars_to_string;

const PROC_PIDFDVNODEPATHINFO: c_int = 2;
const PROC_PIDFDSOCKETINFO: c_int = 3;
const SOCKINFO_IN: c_int = 1;
const SOCKINFO_TCP: c_int = 2;
const SOCKINFO_UN: c_int = 3;
const INI_IPV4: u8 = 0x1;
/// `tcpsi_state` names, in `TSI_S_*` order.
#[rustfmt::skip]
const TCP_STATES: [&str; 11] = [
  "closed", "listen", "syn_sent", "syn_received", "established", "close_wait", "fin_wait_1",
  "closing", "last_ack", "fin_wait_2", "time_wait",
];

/// `struct proc_fileinfo`.
#[repr(C)]
struct proc_fileinfo {
  /// `FREAD` (1), `FWRITE` (2) and other `f_flag` bits.
  fi_openflags: u32,
  _rest: [u32; 5], // fi_status to fi_guardflags
}

/// `struct vnode_fdinfowithpath`.
#[repr(C)]
struct vnode_fdinfowithpath {
  pfi: proc_fileinfo,
  pvip: libc::vnode_info_path,
}

/// `struct socket_fdinfo`.
#[repr(C)]
struct socket_fdinfo {
  _pfi: [u64; 3], // proc_fileinfo
  psi: socket_info,
}

/// `struct socket_info`.
#[repr(C)]
struct socket_info {
  _head: [u64; 19], // soi_stat, soi_so, soi_pcb
  soi_type: c_int,
  _mid: [u32; 19], // soi_protocol to soi_snd
  soi_kind: c_int,
  _rfu_1: u32,
  soi_proto: soi_proto,
}

/// The `soi_proto` union of `socket_info`; `soi_kind` tells which member is filled.
#[repr(C)]
union soi_proto {
  pri_in: in_sockinfo,
  pri_tcp: tcp_sockinfo,
  pri_un: un_sockinfo,
}

/// `struct in_sockinfo`; ports are in network byte order.
#[repr(C)]
#[derive(Clone, Copy)]
struct in_sockinfo {
  insi_fport: c_int,
  insi_lport: c_int,
  _gen: [u64; 2], // insi_gencnt, insi_flags, insi_flow
  insi_vflag: u8,
  _ttl: [u8; 7], // insi_ip_ttl, rfu_1
  /// `in4in6_addr` (IPv4 in the last 4 bytes) or `in6_addr`.
  insi_faddr: [u8; 16],
  insi_laddr: [u8; 16],
  _tail: [u64; 2], // insi_v4, insi_v6
}

/// `struct tcp_sockinfo`.
#[repr(C)]
#[derive(Clone, Copy)]
struct tcp_sockinfo {
  tcpsi_ini: in_sockinfo,
  tcpsi_state: c_int,
  _rest: [u32; 9], // tcpsi_timer to tcpsi_tp
}

/// `struct un_sockinfo`; the addresses are `sockaddr_un` (the path 2 bytes in) in 255-byte unions.
#[repr(C)]
#[derive(Clone, Copy)]
struct un_sockinfo {
  _conn: [u64; 2], // unsi_conn_so, unsi_conn_pcb
  unsi_addr: [c_char; 255],
  unsi_caddr: [c_char; 255],
}

/// What an open file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
  Cwd,
  Dir,
  File,
  /// A character device (`/dev/null`, a tty).
  Dev,
  Tcp,
  Udp,
  Unix,
  Other,
  Pipe,
  Kqueue,
}

impl Kind {
  /// The type column of the info overlay.
  pub fn label(self) -> &'static str {
    let labels = ["cwd", "dir", "file", "dev", "tcp", "udp", "unix", "other", "pipe", "kqueue"];
    labels[self as usize]
  }
}

/// The working directory or an open descriptor of a process.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenFile {
  /// `None` for the working directory.
  pub fd: Option<i32>,
  pub kind: Kind,
  /// Path; `local → remote` for TCP (with its state) and UDP sockets; empty when there is none.
  pub text: String,
  /// `r`, `w` or `rw` for a dir, file or dev descriptor; empty for the rest.
  pub mode: &'static str,
}

/// Reads a `T` with `call(buffer, size)`, a libproc call returning the bytes it wrote:
/// `Err(errno)` when it fails, `Ok(None)` when it wrote another size.
///
/// Safety: `T` is a plain C struct, valid when zeroed and with any bytes the call writes.
unsafe fn read<T>(call: impl FnOnce(*mut c_void, c_int) -> c_int) -> Result<Option<T>, i32> {
  let mut info: T = unsafe { mem::zeroed() };
  let size = mem::size_of::<T>() as c_int;
  match call(&mut info as *mut T as *mut c_void, size) {
    n if n <= 0 => Err(io::Error::last_os_error().raw_os_error().unwrap_or_default()),
    n => Ok((n == size).then_some(info)),
  }
}

/// `proc_pidfdinfo` of one descriptor; `None` when it fails (closed meanwhile) or is cut short.
///
/// Safety: as for `read`.
unsafe fn fd_info<T>(pid: i32, fd: i32, flavor: c_int) -> Option<T> {
  let call = |buf, size| unsafe { libc::proc_pidfdinfo(pid, fd, flavor, buf, size) };
  unsafe { read(call) }.ok().flatten()
}

/// Working directory first, then the descriptors in fd order. `Err(errno)` when the process
/// can't be read: `EPERM` for other users' processes (without root), `ESRCH` when it's gone.
/// A descriptor that can't be read is left out.
pub fn open_files(pid: i32) -> Result<Vec<OpenFile>, i32> {
  let flavor = libc::PROC_PIDVNODEPATHINFO;
  let call = |buf, size| unsafe { libc::proc_pidinfo(pid, flavor, 0, buf, size) };
  let cwd: Option<libc::proc_vnodepathinfo> = unsafe { read(call) }?;

  let mut files = Vec::new();
  if let Some(cwd) = cwd {
    let text = c_chars_to_string(cwd.pvi_cdir.vip_path.as_flattened());
    files.push(OpenFile { fd: None, kind: Kind::Cwd, text, mode: "" });
  }

  // With a NULL buffer it returns the bytes needed; 0 after the cwd call succeeded means no fds.
  // Reserve extra for descriptors opened between the two calls.
  let null = std::ptr::null_mut();
  let bytes = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, null, 0) };
  if bytes <= 0 {
    return Ok(files);
  }

  let entry = libc::PROC_PIDLISTFD_SIZE as usize;
  let mut fds = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; bytes as usize / entry + 32];
  let (buf, size) = (fds.as_mut_ptr() as *mut c_void, (fds.len() * entry) as c_int);
  let bytes = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buf, size) };
  fds.truncate(bytes.max(0) as usize / entry);

  files.extend(fds.iter().filter_map(|fd| {
    let mut mode = "";
    let (kind, text) = match fd.proc_fdtype as c_int {
      libc::PROX_FDTYPE_VNODE => {
        let info: vnode_fdinfowithpath =
          unsafe { fd_info(pid, fd.proc_fd, PROC_PIDFDVNODEPATHINFO) }?;
        mode = ["", "r", "w", "rw"][(info.pfi.fi_openflags & 3) as usize];
        let kind = match info.pvip.vip_vi.vi_stat.vst_mode & libc::S_IFMT {
          libc::S_IFDIR => Kind::Dir,
          libc::S_IFCHR => Kind::Dev,
          _ => Kind::File,
        };
        (kind, c_chars_to_string(info.pvip.vip_path.as_flattened()))
      }
      libc::PROX_FDTYPE_SOCKET => {
        let info: socket_fdinfo = unsafe { fd_info(pid, fd.proc_fd, PROC_PIDFDSOCKETINFO) }?;
        socket(&info.psi)
      }
      libc::PROX_FDTYPE_PIPE => (Kind::Pipe, String::new()),
      libc::PROX_FDTYPE_KQUEUE => (Kind::Kqueue, String::new()),
      _ => (Kind::Other, String::new()),
    };
    Some(OpenFile { fd: Some(fd.proc_fd), kind, text, mode })
  }));
  Ok(files)
}

/// Kind and text of a socket. Unix sockets show their own path, else the peer's.
fn socket(info: &socket_info) -> (Kind, String) {
  let proto = &info.soi_proto;
  // Safety: every member is plain integers, valid with any bytes; `soi_kind` tells which one the
  // kernel filled.
  match info.soi_kind {
    SOCKINFO_TCP => {
      let tcp = unsafe { &proto.pri_tcp };
      let addrs = in_addrs(&tcp.tcpsi_ini);
      match usize::try_from(tcp.tcpsi_state).ok().and_then(|i| TCP_STATES.get(i)) {
        Some(state) => (Kind::Tcp, format!("{addrs} ({state})")),
        None => (Kind::Tcp, addrs),
      }
    }
    SOCKINFO_IN if info.soi_type == libc::SOCK_DGRAM => {
      (Kind::Udp, in_addrs(unsafe { &proto.pri_in }))
    }
    SOCKINFO_UN => {
      let un = unsafe { &proto.pri_un };
      let own = c_chars_to_string(&un.unsi_addr[2..]);
      (Kind::Unix, if own.is_empty() { c_chars_to_string(&un.unsi_caddr[2..]) } else { own })
    }
    _ => (Kind::Other, String::new()),
  }
}

/// `local → remote` of an IP socket, without the remote side while its port is 0 (listeners,
/// unbound UDP).
fn in_addrs(info: &in_sockinfo) -> String {
  let v4 = info.insi_vflag & INI_IPV4 != 0;
  let addr = |bytes: [u8; 16], port: c_int| {
    let [.., a, b, c, d] = bytes;
    let ip: IpAddr =
      if v4 { Ipv4Addr::new(a, b, c, d).into() } else { Ipv6Addr::from(bytes).into() };
    SocketAddr::new(ip, u16::from_be(port as u16))
  };

  let local = addr(info.insi_laddr, info.insi_lport);
  match info.insi_fport {
    0 => local.to_string(),
    port => format!("{local} → {}", addr(info.insi_faddr, port)),
  }
}

#[cfg(test)]
mod tests {
  use std::fs::{self, File};
  use std::net::{TcpListener, TcpStream, UdpSocket};
  use std::os::fd::AsRawFd;
  use std::os::unix::net::{UnixListener, UnixStream};

  use super::*;

  fn own_files() -> Vec<OpenFile> {
    open_files(std::process::id() as i32).expect("own process is readable")
  }

  /// Kind and text of `fd` among the test process' open files.
  fn entry(fd: &impl AsRawFd) -> (Kind, String) {
    let fd = fd.as_raw_fd();
    let file = own_files().into_iter().find(|file| file.fd == Some(fd)).expect("fd is listed");
    (file.kind, file.text)
  }

  #[test]
  fn layout_matches_sdk() {
    assert_eq!(mem::size_of::<libc::proc_fdinfo>(), 8);
    assert_eq!(mem::size_of::<vnode_fdinfowithpath>(), 1200);
    assert_eq!(mem::offset_of!(vnode_fdinfowithpath, pfi.fi_openflags), 0);
    assert_eq!(mem::offset_of!(vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_mode), 28);
    assert_eq!(mem::offset_of!(vnode_fdinfowithpath, pvip.vip_path), 176);
    assert_eq!(mem::size_of::<socket_fdinfo>(), 792);
    assert_eq!(mem::offset_of!(socket_fdinfo, psi), 24);

    assert_eq!(mem::size_of::<socket_info>(), 768);
    assert_eq!(mem::align_of::<socket_info>(), 8);
    assert_eq!(mem::offset_of!(socket_info, soi_type), 152);
    assert_eq!(mem::offset_of!(socket_info, soi_kind), 232);
    assert_eq!(mem::offset_of!(socket_info, soi_proto), 240);
    assert_eq!(mem::size_of::<soi_proto>(), 528);

    assert_eq!(mem::size_of::<in_sockinfo>(), 80);
    assert_eq!(mem::offset_of!(in_sockinfo, insi_lport), 4);
    assert_eq!(mem::offset_of!(in_sockinfo, insi_vflag), 24);
    assert_eq!(mem::offset_of!(in_sockinfo, insi_faddr), 32);
    assert_eq!(mem::offset_of!(in_sockinfo, insi_laddr), 48);
    assert_eq!(mem::size_of::<tcp_sockinfo>(), 120);
    assert_eq!(mem::offset_of!(tcp_sockinfo, tcpsi_state), 80);
    assert_eq!(mem::size_of::<un_sockinfo>(), 528);
    assert_eq!(mem::offset_of!(un_sockinfo, unsi_addr), 16);
    assert_eq!(mem::offset_of!(un_sockinfo, unsi_caddr), 271);
  }

  #[test]
  fn cwd_comes_first() {
    let cwd = std::env::current_dir().unwrap();
    let text = cwd.to_string_lossy().into_owned();
    assert_eq!(own_files()[0], OpenFile { fd: None, kind: Kind::Cwd, text, mode: "" });
  }

  #[test]
  fn lists_an_open_file() {
    let path = std::env::temp_dir().join(format!("macmon-fds-{}", std::process::id()));
    let file = File::create(&path).unwrap();
    let listed = entry(&file);
    let real = fs::canonicalize(&path).unwrap(); // /var is a symlink to /private/var
    fs::remove_file(&path).unwrap();
    assert_eq!(listed, (Kind::File, real.to_string_lossy().into_owned()));
  }

  #[test]
  fn tells_dirs_and_devices_and_the_open_mode() {
    let kind_mode = |fd: &dyn AsRawFd| {
      let fd = fd.as_raw_fd();
      let file = own_files().into_iter().find(|file| file.fd == Some(fd)).expect("fd is listed");
      (file.kind, file.mode)
    };
    let null = File::options().read(true).write(true).open("/dev/null").unwrap();
    assert_eq!(kind_mode(&null), (Kind::Dev, "rw"));
    let dir = File::open(std::env::temp_dir()).unwrap();
    assert_eq!(kind_mode(&dir), (Kind::Dir, "r"));

    let path = std::env::temp_dir().join(format!("macmon-fds-mode-{}", std::process::id()));
    let file = File::options().write(true).create(true).truncate(true).open(&path).unwrap();
    let listed = kind_mode(&file);
    fs::remove_file(&path).unwrap();
    assert_eq!(listed, (Kind::File, "w"));
    // sockets have no mode
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    assert_eq!(kind_mode(&udp), (Kind::Udp, ""));
  }

  #[test]
  fn lists_sockets() {
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = tcp.local_addr().unwrap();
    assert_eq!(entry(&tcp), (Kind::Tcp, format!("{addr} (listen)")));
    let client = TcpStream::connect(addr).unwrap();
    let local = client.local_addr().unwrap();
    assert_eq!(entry(&client), (Kind::Tcp, format!("{local} → {addr} (established)")));

    // a host without IPv6 can't bind ::1
    if let Ok(tcp6) = TcpListener::bind("[::1]:0") {
      let addr = tcp6.local_addr().unwrap();
      assert_eq!(entry(&tcp6), (Kind::Tcp, format!("{addr} (listen)")));
    }

    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = udp.local_addr().unwrap();
    assert_eq!(entry(&udp), (Kind::Udp, addr.to_string()));

    let path = std::env::temp_dir().join(format!("macmon-fds-{}.sock", std::process::id()));
    let _ = fs::remove_file(&path);
    let unix = UnixListener::bind(&path).unwrap();
    // an unbound client shows its peer's path
    let peer = UnixStream::connect(&path).unwrap();
    let listed = [entry(&unix), entry(&peer)];
    fs::remove_file(&path).unwrap();
    let path = path.to_string_lossy().into_owned();
    assert_eq!(listed, [(Kind::Unix, path.clone()), (Kind::Unix, path)]);
  }

  #[test]
  fn errors_of_unreadable_processes() {
    if unsafe { libc::geteuid() } != 0 {
      assert_eq!(open_files(1), Err(libc::EPERM)); // launchd belongs to root
    }
    assert_eq!(open_files(i32::MAX), Err(libc::ESRCH));
  }
}
