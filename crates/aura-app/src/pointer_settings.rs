use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask};
use x11rb::resource_manager::Database;
use x11rb::xcb_ffi::XCBConnection;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    theme: Option<String>,
    size: Option<u32>,
    dpi: Option<u32>,
}

impl Values {
    fn fallback(self, next: Self) -> Self {
        Self {
            theme: self.theme.or(next.theme),
            size: self.size.or(next.size),
            dpi: self.dpi.or(next.dpi),
        }
    }

    pub fn database(&self) -> Database {
        let mut text = String::new();
        if let Some(theme) = &self.theme {
            text.push_str(&format!("Xcursor.theme: {theme}\n"));
        }
        if let Some(size) = self.size {
            text.push_str(&format!("Xcursor.size: {size}\n"));
        }
        if let Some(dpi) = self.dpi {
            text.push_str(&format!("Xft.dpi: {dpi}\n"));
        }
        Database::new_from_data(text.as_bytes())
    }
}

fn theme(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn size(value: u32) -> Option<u32> {
    (1..=1024).contains(&value).then_some(value)
}

fn gtk_values(text: &str) -> Values {
    let mut settings = false;
    let mut values = Values::default();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            settings = line == "[Settings]";
        } else if settings && let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "gtk-cursor-theme-name" => values.theme = theme(value),
                "gtk-cursor-theme-size" => values.size = value.trim().parse().ok().and_then(size),
                _ => {}
            }
        }
    }
    values
}

fn gtk_paths() -> Vec<PathBuf> {
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let directories = std::env::var_os("XDG_CONFIG_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/etc/xdg".into());
    user.into_iter()
        .chain(std::env::split_paths(&directories))
        .chain([PathBuf::from("/etc")])
        .flat_map(|directory| {
            ["gtk-3.0/settings.ini", "gtk-4.0/settings.ini"].map(|name| directory.join(name))
        })
        .collect()
}

#[derive(PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    length: u64,
    inode: u64,
}

pub struct Settings {
    root: u32,
    selection: u32,
    property: u32,
    manager: u32,
    owner: u32,
    paths: Vec<PathBuf>,
    stamps: Vec<Option<Stamp>>,
}

impl Settings {
    pub fn new(
        connection: &XCBConnection,
        screen: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let root = connection.setup().roots[screen].root;
        let mask = connection
            .get_window_attributes(root)?
            .reply()?
            .your_event_mask;
        connection
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new()
                    .event_mask(mask | EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
            )?
            .check()?;
        let selection = connection
            .intern_atom(false, format!("_XSETTINGS_S{screen}").as_bytes())?
            .reply()?
            .atom;
        let property = connection
            .intern_atom(false, b"_XSETTINGS_SETTINGS")?
            .reply()?
            .atom;
        let manager = connection.intern_atom(false, b"MANAGER")?.reply()?.atom;
        Ok(Self {
            root,
            selection,
            property,
            manager,
            owner: x11rb::NONE,
            paths: gtk_paths(),
            stamps: Vec::new(),
        })
    }

    pub fn handles(&self, event: &Event) -> bool {
        match event {
            Event::PropertyNotify(event) => {
                (event.window == self.root && event.atom == u32::from(AtomEnum::RESOURCE_MANAGER))
                    || (event.window == self.owner && event.atom == self.property)
            }
            Event::ClientMessage(event) => {
                event.window == self.root
                    && event.type_ == self.manager
                    && event.format == 32
                    && event.data.as_data32()[1] == self.selection
            }
            Event::DestroyNotify(event) => self.owner != x11rb::NONE && event.window == self.owner,
            _ => false,
        }
    }

    /// Uses the existing configuration tick, never a frame or keypress path.
    pub fn files_changed(&mut self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let stamps = self
            .paths
            .iter()
            .map(|path| {
                std::fs::metadata(path).ok().map(|value| Stamp {
                    modified: value.modified().ok(),
                    length: value.len(),
                    inode: value.ino(),
                })
            })
            .collect();
        if self.stamps == stamps {
            false
        } else {
            self.stamps = stamps;
            true
        }
    }

    pub fn read(
        &mut self,
        connection: &XCBConnection,
    ) -> Result<Values, Box<dyn std::error::Error>> {
        let resources = x11rb::resource_manager::new_from_default(connection)?;
        let explicit = Values {
            theme: resources.get_string("Xcursor.theme", "").and_then(theme),
            size: resources
                .get_value("Xcursor.size", "")
                .ok()
                .flatten()
                .and_then(size),
            dpi: resources
                .get_value("Xft.dpi", "")
                .ok()
                .flatten()
                .filter(|dpi| (1..=10_000).contains(dpi)),
        };
        let owner = connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner;
        if owner != self.owner {
            self.owner = owner;
            if owner != x11rb::NONE
                && let Ok(attributes) = connection.get_window_attributes(owner)?.reply()
            {
                // A settings manager can disappear between the selection and
                // property requests; that must not close a terminal session.
                let _ = connection
                    .change_window_attributes(
                        owner,
                        &ChangeWindowAttributesAux::new().event_mask(
                            attributes.your_event_mask
                                | EventMask::PROPERTY_CHANGE
                                | EventMask::STRUCTURE_NOTIFY,
                        ),
                    )?
                    .check();
            }
        }
        let desktop = if owner == x11rb::NONE {
            Values::default()
        } else {
            connection
                .get_property(false, owner, self.property, self.property, 0, 262_144)?
                .reply()
                .ok()
                .filter(|reply| {
                    reply.format == 8 && reply.type_ == self.property && reply.bytes_after == 0
                })
                .and_then(|reply| xsettings_values(&reply.value))
                .unwrap_or_default()
        };
        let mut gtk = Values::default();
        for path in &self.paths {
            let mut text = String::new();
            if let Ok(file) = std::fs::File::open(path)
                && file.take(65_537).read_to_string(&mut text).is_ok()
                && text.len() <= 65_536
            {
                gtk = gtk.fallback(gtk_values(&text));
            }
        }
        self.files_changed();
        // x11rb applies XCURSOR_THEME/SIZE over this database. Preserve explicit
        // Xresources, then desktop settings, then GTK defaults (GTK3 before GTK4).
        Ok(explicit.fallback(desktop).fallback(gtk))
    }
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
    little: bool,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let bytes = self.data.get(self.offset..end)?;
        self.offset = end;
        Some(bytes)
    }

    fn pad(&mut self) -> Option<()> {
        self.take((4 - self.offset % 4) % 4).map(|_| ())
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }
}

fn xsettings_values(data: &[u8]) -> Option<Values> {
    if data.len() > 1_048_576 {
        return None;
    }
    let little = match data.first()? {
        0 => true,
        1 => false,
        _ => return None,
    };
    let mut reader = Reader {
        data,
        offset: 4,
        little,
    };
    reader.u32()?; // Selection serial.
    let count = reader.u32()?;
    if count > 4096 {
        return None;
    }
    let mut result = Values::default();
    for _ in 0..count {
        let kind = reader.take(2)?[0];
        let length = usize::from(reader.u16()?);
        let name = reader.take(length)?;
        reader.pad()?;
        reader.u32()?; // Last-change serial.
        match kind {
            0 => {
                let value = reader.u32()?;
                if name == b"Gtk/CursorThemeSize" {
                    result.size = size(value);
                }
            }
            1 => {
                let length = usize::try_from(reader.u32()?).ok()?;
                let value = reader.take(length)?;
                reader.pad()?;
                if name == b"Gtk/CursorThemeName" {
                    result.theme = std::str::from_utf8(value).ok().and_then(theme);
                }
            }
            2 => {
                reader.take(8)?;
            }
            _ => return None,
        }
    }
    (reader.offset == data.len()).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_settings_obey_sections_and_ignore_invalid_cursor_values() {
        let values = gtk_values(
            "[Other]\ngtk-cursor-theme-name=wrong\n[Settings]\n# comment\ngtk-cursor-theme-name= Adwaita \ngtk-cursor-theme-size=24\n",
        );
        assert_eq!(values.theme.as_deref(), Some("Adwaita"));
        assert_eq!(values.size, Some(24));
        for value in ["-1", "0", "1025", "4294967296", "large"] {
            assert_eq!(
                gtk_values(&format!("[Settings]\ngtk-cursor-theme-size={value}")).size,
                None
            );
        }
        assert!(theme("a\rXcursor.size: 2").is_none());
        assert!(theme(&"x".repeat(257)).is_none());
    }

    #[test]
    fn explicit_resources_keep_per_field_precedence_and_dpi() {
        let explicit = Values {
            theme: Some("custom".into()),
            dpi: Some(144),
            ..Values::default()
        };
        let desktop = Values {
            theme: Some("desktop".into()),
            size: Some(48),
            dpi: None,
        };
        let gtk = Values {
            theme: Some("Adwaita".into()),
            size: Some(24),
            dpi: None,
        };
        let database = explicit.fallback(desktop).fallback(gtk).database();
        assert_eq!(database.get_string("Xcursor.theme", ""), Some("custom"));
        assert_eq!(
            database.get_value::<u32>("Xcursor.size", "").unwrap(),
            Some(48)
        );
        assert_eq!(database.get_value::<u32>("Xft.dpi", "").unwrap(), Some(144));
    }

    #[test]
    fn xsettings_accept_both_byte_orders_and_reject_truncation_or_unknown_types() {
        for little in [true, false] {
            let u16bytes = |v: u16| {
                if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };
            let u32bytes = |v: u32| {
                if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };
            let mut data = vec![u8::from(!little), 0, 0, 0];
            data.extend(u32bytes(1));
            data.extend(u32bytes(2));
            for (kind, name) in [(1, "Gtk/CursorThemeName"), (0, "Gtk/CursorThemeSize")] {
                data.extend([kind, 0]);
                data.extend(u16bytes(name.len().try_into().unwrap()));
                data.extend(name.as_bytes());
                while data.len() % 4 != 0 {
                    data.push(0);
                }
                data.extend(u32bytes(1));
                if kind == 1 {
                    data.extend(u32bytes(7));
                    data.extend(b"Adwaita\0");
                } else {
                    data.extend(u32bytes(24));
                }
            }
            assert_eq!(
                xsettings_values(&data).unwrap(),
                Values {
                    theme: Some("Adwaita".into()),
                    size: Some(24),
                    dpi: None
                }
            );
            for end in 0..data.len() {
                assert!(xsettings_values(&data[..end]).is_none());
            }
            data[12] = 9;
            assert!(xsettings_values(&data).is_none());
        }
    }

    #[test]
    fn xsettings_reject_oversized_counts_and_lengths_without_partial_values() {
        let mut data = vec![0; 12];
        data[8..12].copy_from_slice(&4097u32.to_le_bytes());
        assert!(xsettings_values(&data).is_none());
        data[0] = 2;
        assert!(xsettings_values(&data).is_none());
        data[0] = 0;
        data[8..12].copy_from_slice(&1u32.to_le_bytes());
        data.extend([1, 0, 0, 0]);
        data.extend(0u32.to_le_bytes());
        data.extend(u32::MAX.to_le_bytes());
        assert!(xsettings_values(&data).is_none());
        assert!(xsettings_values(&vec![0; 1_048_577]).is_none());
    }
}
