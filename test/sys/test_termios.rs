use std::os::unix::io::AsFd;
use tempfile::tempfile;

use nix::errno::Errno;
use nix::fcntl;
use nix::pty::openpty;
use nix::sys::termios::{self, tcgetattr, BaudRate, LocalFlags, OutputFlags};
use nix::unistd::{read, write};

/// Helper function analogous to `std::io::Write::write_all`, but for `Fd`s
fn write_all<Fd: AsFd>(f: Fd, buf: &[u8]) {
    let mut len = 0;
    while len < buf.len() {
        len += write(f.as_fd(), &buf[len..]).unwrap();
    }
}

#[test]
fn test_baudrate_try_from() {
    assert_eq!(Ok(BaudRate::B0), BaudRate::try_from(libc::B0));
    #[cfg(not(target_os = "haiku"))]
    BaudRate::try_from(999999999).expect_err("assertion failed");
    #[cfg(target_os = "haiku")]
    BaudRate::try_from(99).expect_err("assertion failed");
}

// Test tcgetattr on a terminal
#[test]
fn test_tcgetattr_pty() {
    // openpty uses ptname(3) internally
    let _m = crate::PTSNAME_MTX.lock();

    let pty = openpty(None, None).expect("openpty failed");
    termios::tcgetattr(&pty.slave).unwrap();
}

// Test tcgetattr on something that isn't a terminal
#[test]
fn test_tcgetattr_enotty() {
    let file = tempfile().unwrap();
    assert_eq!(termios::tcgetattr(&file).err(), Some(Errno::ENOTTY));
}

// Test modifying output flags
#[test]
fn test_output_flags() {
    // openpty uses ptname(3) internally
    let _m = crate::PTSNAME_MTX.lock();

    // Open one pty to get attributes for the second one
    let mut termios = {
        let pty = openpty(None, None).expect("openpty failed");
        tcgetattr(&pty.slave).expect("tcgetattr failed")
    };

    // Make sure postprocessing '\r' isn't specified by default or this test is useless.
    assert!(!termios
        .output_flags
        .contains(OutputFlags::OPOST | OutputFlags::OCRNL));

    // Specify that '\r' characters should be transformed to '\n'
    // OPOST is specified to enable post-processing
    termios
        .output_flags
        .insert(OutputFlags::OPOST | OutputFlags::OCRNL);

    // Open a pty
    let pty = openpty(None, &termios).unwrap();

    // Write into the master
    let string = "foofoofoo\r";
    write_all(&pty.master, string.as_bytes());

    // Read from the slave verifying that the output has been properly transformed
    let mut buf = [0u8; 10];
    crate::read_exact(&pty.slave, &mut buf);
    let transformed_string = "foofoofoo\n";
    assert_eq!(&buf, transformed_string.as_bytes());
}

// Test modifying local flags
#[test]
#[cfg(not(target_os = "solaris"))]
fn test_local_flags() {
    // openpty uses ptname(3) internally
    let _m = crate::PTSNAME_MTX.lock();

    // Open one pty to get attributes for the second one
    let mut termios = {
        let pty = openpty(None, None).unwrap();
        tcgetattr(&pty.slave).unwrap()
    };

    // Make sure echo is specified by default or this test is useless.
    assert!(termios.local_flags.contains(LocalFlags::ECHO));

    // Disable local echo
    termios.local_flags.remove(LocalFlags::ECHO);

    // Open a new pty with our modified termios settings
    let pty = openpty(None, &termios).unwrap();

    // Set the master is in nonblocking mode or reading will never return.
    let flags = fcntl::fcntl(&pty.master, fcntl::F_GETFL).unwrap();
    let new_flags =
        fcntl::OFlag::from_bits_truncate(flags) | fcntl::OFlag::O_NONBLOCK;
    fcntl::fcntl(pty.master.as_fd(), fcntl::F_SETFL(new_flags)).unwrap();

    // Write into the master
    let string = "foofoofoo\r";
    write_all(&pty.master, string.as_bytes());

    // Try to read from the master, which should not have anything as echoing was disabled.
    let mut buf = [0u8; 10];
    let read = read(&pty.master, &mut buf).unwrap_err();
    assert_eq!(read, Errno::EAGAIN);
}

// An unchanged round trip must not discard settings unknown to Nix, such as
// baud-rate bits in c_cflag on illumos. Use a disposable PTY, not the test TTY.
// BSD speed getters return u32; other targets return BaudRate.
#[allow(clippy::useless_conversion)]
#[test]
fn test_tcsetattr_preserves_flags() {
    use std::os::fd::AsRawFd;

    let _m = crate::PTSNAME_MTX.lock();
    let pty = openpty(None, None).unwrap();
    let fd = pty.slave.as_raw_fd();
    let mut raw = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(unsafe { libc::tcgetattr(fd, raw.as_mut_ptr()) }, 0);
    let mut raw = unsafe { raw.assume_init() };
    assert_eq!(unsafe { libc::cfsetospeed(&mut raw, libc::B38400) }, 0);
    assert_eq!(unsafe { libc::cfsetispeed(&mut raw, libc::B38400) }, 0);
    #[cfg(solarish)]
    {
        // TAB3 (0014000 in illumos sys/termios.h) is not exposed by libc.
        raw.c_oflag |= 0o14000;
    }
    assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) }, 0);
    // Read back the settings actually accepted by the driver.
    assert_eq!(unsafe { libc::tcgetattr(fd, &mut raw) }, 0);

    let termios = tcgetattr(&pty.slave).unwrap();
    assert_eq!(termios::cfgetospeed(&termios), BaudRate::B38400.into());
    assert_eq!(termios::cfgetispeed(&termios), BaudRate::B38400.into());
    termios::tcsetattr(&pty.slave, termios::SetArg::TCSANOW, &termios).unwrap();

    let mut after = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(unsafe { libc::tcgetattr(fd, after.as_mut_ptr()) }, 0);
    let after = unsafe { after.assume_init() };
    assert_eq!(after.c_iflag, raw.c_iflag);
    assert_eq!(after.c_oflag, raw.c_oflag);
    assert_eq!(after.c_cflag, raw.c_cflag);
    assert_eq!(after.c_lflag, raw.c_lflag);
    assert_eq!(unsafe { libc::cfgetospeed(&after) }, libc::B38400);
    assert_eq!(unsafe { libc::cfgetispeed(&after) }, libc::B38400);
}
