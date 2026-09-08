// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Counterpart of `frontend_common/settings_generator.{h,cpp}`.

/// Generate absent frontend identity fields after loading configuration.
pub fn generate_settings() {
    let mut generator = common::random::get_mt19937();
    generate_settings_with_rng(&mut common::settings::values_mut(), &mut || {
        generator.next_u32()
    });
}

// Mechanical test seam for GenerateSettings, keeping its state and ordering in
// the upstream owner. Existing values never consume distribution samples.
fn generate_settings_with_rng(
    values: &mut common::settings::Values,
    random: &mut impl FnMut() -> u32,
) {
    if values.eden_token.get_value().is_empty() {
        const TOKEN_LENGTH: usize = 48;
        const TOKEN_SET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
        let mut token = String::with_capacity(TOKEN_LENGTH);
        for _ in 0..TOKEN_LENGTH {
            let index = uniform_int_distribution(random, 0, TOKEN_SET.len() as u32 - 1);
            token.push(TOKEN_SET[index as usize] as char);
        }
        values.eden_token.set_value(token);
    }
    if *values.serial_unit.get_value() == 0 {
        values
            .serial_unit
            .set_value(uniform_int_distribution(random, 1, u32::MAX));
    }
    if *values.serial_battery.get_value() == 0 {
        values
            .serial_battery
            .set_value(uniform_int_distribution(random, 1, u32::MAX));
    }
}

// std::uniform_int_distribution guarantees an inclusive uniform distribution,
// not a portable sequence across standard libraries. Rejection avoids modulo
// bias; MT19937 remains the same upstream engine, with no extra dependency.
fn uniform_int_distribution(random: &mut impl FnMut() -> u32, min: u32, max: u32) -> u32 {
    let range = u64::from(max) - u64::from(min) + 1;
    let accepted = (1u64 << 32) / range * range;
    loop {
        let value = u64::from(random());
        if value < accepted {
            return (u64::from(min) + value % range) as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_is_nonzero_and_preserves_existing_values() {
        let mut values = common::settings::Values::default();
        values
            .yuzu_token
            .set_value("existing-credential".to_string());
        let mut generator = common::random::Mt19937::new(5489);
        generate_settings_with_rng(&mut values, &mut || generator.next_u32());
        assert_eq!(values.eden_token.get_value().len(), 48);
        assert!(values
            .eden_token
            .get_value()
            .bytes()
            .all(|byte| byte.is_ascii_lowercase()));
        assert_ne!(*values.serial_battery.get_value(), 0);
        assert_ne!(*values.serial_unit.get_value(), 0);
        assert_eq!(values.yuzu_token.get_value(), "existing-credential");
        generate_settings_with_rng(&mut values, &mut || {
            panic!("existing identity must be preserved")
        });

        values.serial_unit.set_value(0);
        let battery = *values.serial_battery.get_value();
        let token = values.eden_token.get_value().clone();
        let mut samples = [u32::MAX, u32::MAX - 1].into_iter();
        generate_settings_with_rng(&mut values, &mut || samples.next().unwrap());
        assert_eq!(*values.serial_unit.get_value(), u32::MAX);
        assert_eq!(*values.serial_battery.get_value(), battery);
        assert_eq!(values.eden_token.get_value(), &token);
        assert!(samples.next().is_none());
    }

    #[test]
    fn distributions_reject_biased_tail_and_include_endpoints() {
        let mut samples = [u32::MAX, 0, 25].into_iter();
        assert_eq!(
            uniform_int_distribution(&mut || samples.next().unwrap(), 0, 25),
            0
        );
        assert_eq!(
            uniform_int_distribution(&mut || samples.next().unwrap(), 0, 25),
            25
        );
        assert_eq!(uniform_int_distribution(&mut || 0, 1, u32::MAX), 1);
    }
}
