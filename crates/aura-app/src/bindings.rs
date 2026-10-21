use crate::keyboard::{FontSizeAction, font_size_action};
use aura_config::{BindingAction as Action, KeyboardConfig};
use x11rb::protocol::xproto::KeyButMask;
use xkbcommon::xkb;

pub struct Bindings {
    custom: Vec<(u32, u8, u8, Action)>,
    defaults: bool,
}
impl Bindings {
    pub fn new(config: &KeyboardConfig) -> Result<Self, std::io::Error> {
        let custom = config
            .bindings
            .iter()
            .map(|binding| {
                let sym = if binding.key.chars().count() == 1 {
                    xkb::utf32_to_keysym(binding.key.chars().next().unwrap() as u32).raw()
                } else {
                    xkb::keysym_from_name(&binding.key, xkb::KEYSYM_CASE_INSENSITIVE).raw()
                };
                if sym == 0 {
                    return Err(std::io::Error::other(format!(
                        "unknown keyboard binding key {}",
                        binding.key
                    )));
                }
                Ok((
                    normalize(sym),
                    binding.mods,
                    binding.screen,
                    binding.action.clone(),
                ))
            })
            .collect::<Result<_, std::io::Error>>()?;
        Ok(Self {
            custom,
            defaults: config.use_default_bindings,
        })
    }
    pub fn action(&self, sym: u32, state: KeyButMask, alternate: bool) -> Option<Action> {
        let mods = u8::from(state.contains(KeyButMask::SHIFT))
            + 2 * u8::from(state.contains(KeyButMask::MOD1))
            + 4 * u8::from(state.contains(KeyButMask::CONTROL))
            + 8 * u8::from(state.contains(KeyButMask::MOD4));
        for (key, mask, screen, action) in self.custom.iter().rev() {
            if *key == normalize(sym)
                && *mask == mods
                && (*screen == 0 || (*screen == 2) == alternate)
            {
                return Some(action.clone());
            }
        }
        if !self.defaults {
            return None;
        }
        if let Some(action) = font_size_action(sym, state) {
            return Some(match action {
                FontSizeAction::Grow => Action::IncreaseFontSize,
                FontSizeAction::Shrink => Action::DecreaseFontSize,
                FontSizeAction::Reset => Action::ResetFontSize,
            });
        }
        match (normalize(sym), mods) {
            (0x63, 5) => Some(Action::Copy),
            (0x76, 5) => Some(Action::Paste),
            (0x72, 5) => Some(Action::ReloadConfig),
            (0xff63, 1) => Some(Action::PasteSelection),
            (0xff55, 1) if !alternate => Some(Action::ScrollPageUp),
            (0xff56, 1) if !alternate => Some(Action::ScrollPageDown),
            (0xff50, 5) if !alternate => Some(Action::ScrollTop),
            (0xff57, 5) if !alternate => Some(Action::ScrollBottom),
            _ => None,
        }
    }
}
fn normalize(sym: u32) -> u32 {
    if (0x41..=0x5a).contains(&sym) {
        sym + 32
    } else {
        sym
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_bindings_override_defaults_without_consuming_other_modes() {
        let table=toml::from_str("[keyboard]\n[[keyboard.bindings]]\nkey='C'\nmods='Control|Shift'\naction='None'\n[[keyboard.bindings]]\nkey='Return'\nmods='Shift'\nmode='Alternate'\nchars='custom'").unwrap();
        let config = aura_config::AppConfig::load_from_text(&table).unwrap();
        let bindings = Bindings::new(&config.keyboard).unwrap();
        assert_eq!(
            bindings.action(0x43, KeyButMask::CONTROL | KeyButMask::SHIFT, false),
            Some(Action::None)
        );
        assert_eq!(
            bindings.action(0xff0d, KeyButMask::SHIFT, true),
            Some(Action::Send("custom".into()))
        );
        assert_eq!(bindings.action(0xff0d, KeyButMask::SHIFT, false), None);
        assert!(bindings.action(0xff55, KeyButMask::SHIFT, true).is_none());
    }
}
