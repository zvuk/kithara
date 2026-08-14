#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::ci::{
    config::{CiConfig, WindowsGuest},
    process::Process,
};

/// What the guest boots into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Boot {
    /// Boot the installed disk.
    Installed,
    /// Boot the Microsoft media with the answer file attached.
    Install,
}

/// The Windows guest served by a mac, and the disks it is handed.
///
/// This replaces a shell script that lived beside the disk image on the CI
/// volume, outside the repository, along with two dozen probe scripts from the
/// session that built the guest. Nothing outside the repository is reviewed,
/// reproducible, or reachable by a fix, which is the same reason the periodic
/// agents stopped running a binary only an operator could replace.
pub(super) struct WindowsHost<'a> {
    guest: &'a WindowsGuest,
    root: PathBuf,
    process: &'a Process,
}

impl<'a> WindowsHost<'a> {
    const QEMU: &'static str = "/opt/homebrew/bin/qemu-system-aarch64";
    const QEMU_IMG: &'static str = "/opt/homebrew/bin/qemu-img";

    pub(super) fn new(config: &'a CiConfig, process: &'a Process) -> Result<Self> {
        let guest = config
            .host
            .windows
            .as_ref()
            .context("this machine's profile defines no Windows guest")?;
        Ok(Self {
            guest,
            root: config.host.host_root.join("vm/windows"),
            process,
        })
    }

    fn system_disk(&self) -> PathBuf {
        self.root.join("disk.qcow2")
    }

    fn data_disk(&self) -> PathBuf {
        self.root.join("data.qcow2")
    }

    /// Create the data disk if it is not there yet.
    ///
    /// Creating it is cheap and does not start the guest: a `qcow2` allocates
    /// nothing until something writes. The guest picks it up on its next boot.
    pub(super) fn ensure_data_disk(&self) -> Result<()> {
        let disk = self.data_disk();
        if disk.exists() {
            return Ok(());
        }
        self.process.run(
            Self::QEMU_IMG,
            &[
                "create",
                "-f",
                "qcow2",
                &disk.display().to_string(),
                &format!("{}G", self.guest.data_disk_gib),
            ],
            "create the Windows guest's data disk",
        )?;
        Ok(())
    }

    /// The arguments that boot the guest.
    pub(super) fn boot_arguments(&self, boot: Boot) -> Vec<String> {
        let root = self.root.display().to_string();
        let mut arguments: Vec<String> = [
            "-name",
            "kithara-windows",
            "-M",
            "virt,highmem=on",
            "-accel",
            "hvf",
            "-cpu",
            "host",
        ]
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
        arguments.extend([
            "-smp".to_owned(),
            self.guest.vcpus.to_string(),
            "-m".to_owned(),
            self.guest.memory_mib.to_string(),
            "-drive".to_owned(),
            format!("if=pflash,format=raw,file={root}/efi-code.fd,readonly=on"),
            "-drive".to_owned(),
            format!("if=pflash,format=raw,file={root}/efi-vars.fd"),
            "-device".to_owned(),
            "ramfb".to_owned(),
            "-device".to_owned(),
            "qemu-xhci,id=usb".to_owned(),
            "-device".to_owned(),
            "usb-kbd".to_owned(),
            "-device".to_owned(),
            "usb-tablet".to_owned(),
        ]);
        // `discard=unmap` on both, so a guest that deletes a file lets the
        // image hand the blocks back. Without it the image only ever grows,
        // which is how the Linux guest's data disk reached a hundred nominal
        // gigabytes holding fourteen.
        arguments.extend(Self::disk(
            "disk",
            &self.system_disk().display().to_string(),
            "kithara",
        ));
        arguments.extend(Self::disk(
            "data",
            &self.data_disk().display().to_string(),
            "kithara-data",
        ));
        arguments.extend([
            "-netdev".to_owned(),
            "user,id=net0,hostfwd=tcp::2222-:22".to_owned(),
            "-device".to_owned(),
            "virtio-net-pci,netdev=net0".to_owned(),
            "-drive".to_owned(),
            format!("if=none,id=cd2,media=cdrom,file={root}/virtio-win.iso,readonly=on"),
            "-device".to_owned(),
            "usb-storage,drive=cd2".to_owned(),
            "-rtc".to_owned(),
            "base=utc".to_owned(),
            "-display".to_owned(),
            "none".to_owned(),
            "-monitor".to_owned(),
            format!("unix:{root}/monitor.sock,server,nowait"),
            "-pidfile".to_owned(),
            format!("{root}/qemu.pid"),
        ]);
        if boot == Boot::Install {
            arguments.extend([
                "-drive".to_owned(),
                format!("if=none,id=cd0,media=cdrom,file={root}/Win11_25H2_Arm64.iso,readonly=on"),
                "-device".to_owned(),
                "usb-storage,drive=cd0,bootindex=0".to_owned(),
                "-drive".to_owned(),
                format!("if=none,id=cd1,media=cdrom,file={root}/unattend.iso,readonly=on"),
                "-device".to_owned(),
                "usb-storage,drive=cd1".to_owned(),
                "-boot".to_owned(),
                "menu=on".to_owned(),
            ]);
        }
        arguments
    }

    fn disk(id: &str, file: &str, serial: &str) -> Vec<String> {
        vec![
            "-drive".to_owned(),
            format!("if=none,id={id},file={file},format=qcow2,cache=writeback,discard=unmap"),
            "-device".to_owned(),
            format!("nvme,drive={id},serial={serial}"),
        ]
    }

    /// Start the guest, replacing a monitor socket a previous run left behind.
    pub(super) fn start(&self, boot: Boot) -> Result<()> {
        self.ensure_data_disk()?;
        let socket = self.root.join("monitor.sock");
        if socket.exists() {
            std::fs::remove_file(&socket)
                .with_context(|| format!("removing stale monitor socket {}", socket.display()))?;
        }
        let owned = self.boot_arguments(boot);
        let arguments: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.process
            .run(Self::QEMU, &arguments, "start the Windows guest")?;
        Ok(())
    }

    #[cfg(test)]
    fn for_test(guest: &'a WindowsGuest, root: &Path, process: &'a Process) -> Self {
        Self {
            guest,
            root: root.to_path_buf(),
            process,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn guest() -> WindowsGuest {
        WindowsGuest {
            vcpus: 4,
            memory_mib: 8192,
            data_disk_gib: 80,
        }
    }

    fn arguments(boot: Boot) -> Vec<String> {
        let guest = guest();
        let process = Process::new(Path::new("/Volumes/CI"), BTreeMap::new());
        WindowsHost::for_test(&guest, Path::new("/Volumes/CI/vm/windows"), &process)
            .boot_arguments(boot)
    }

    /// The page file and `TEMP` are what grow the system image, and Windows
    /// sizes the page file to memory. On its own disk they stop charging the
    /// image the host cannot shrink in place.
    #[test]
    fn what_the_guest_writes_lands_on_a_disk_of_its_own() {
        assert!(
            arguments(Boot::Installed)
                .iter()
                .any(|argument| { argument.contains("file=/Volumes/CI/vm/windows/data.qcow2") })
        );
    }

    /// Without it a guest that deletes a file keeps the blocks forever, which
    /// is the whole reason the system image sits at 78 GB holding far less.
    #[test]
    fn the_data_disk_returns_the_blocks_the_guest_frees() {
        let data = arguments(Boot::Installed)
            .into_iter()
            .find(|argument| argument.contains("data.qcow2"))
            .expect("the guest is handed a data disk");

        assert!(data.contains("discard=unmap"));
    }

    #[test]
    fn the_two_disks_are_told_apart_by_serial() {
        let serials: Vec<String> = arguments(Boot::Installed)
            .into_iter()
            .filter(|argument| argument.starts_with("nvme,"))
            .collect();

        assert_eq!(serials.len(), 2);
    }

    #[test]
    fn installing_attaches_the_answer_file() {
        assert!(
            arguments(Boot::Install)
                .iter()
                .any(|argument| argument.contains("unattend.iso"))
        );
    }

    /// A boot of the installed disk that offered the media as well would
    /// reinstall the guest over itself.
    #[test]
    fn booting_the_installed_disk_offers_no_media() {
        assert!(
            !arguments(Boot::Installed)
                .iter()
                .any(|argument| argument.contains("Win11_25H2_Arm64.iso"))
        );
    }

    #[test]
    fn the_guest_is_given_the_memory_its_profile_asks_for() {
        let arguments = arguments(Boot::Installed);
        let memory = arguments
            .iter()
            .position(|argument| argument == "-m")
            .expect("memory is passed");

        assert_eq!(arguments[memory + 1], "8192");
    }
}
