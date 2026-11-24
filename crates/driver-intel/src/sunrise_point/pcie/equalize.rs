// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PCIe Gen3 equalization, real HSIO jitter/error measurements.
//! No calibration callback, synthetic margin or default 'measured' winner.
use super::{registers::*, *};
use tock_registers::{
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
};
struct Eq<'a> {
    port: &'a HardwarePort,
    pcr: &'a Pcr,
    lanes: [Option<hsio::Lane>; 4],
    width: usize,
    max: usize,
    best: [Option<(usize, u8)>; 4],
}
fn best_measured(previous: Option<(usize, u8)>, index: usize, margin: u8) -> Option<(usize, u8)> {
    if margin > 0 && previous.is_none_or(|(_, old)| margin > old) {
        Some((index, margin))
    } else {
        previous
    }
}
impl Eq<'_> {
    fn freeze(&self, index: usize, on: bool) -> Result<(), Error> {
        let lane = self.lanes[index].ok_or(Error::Topology)?;
        self.pcr
            .register::<Vga::Register>(lane.port, lane.base + 0x15c)?
            .modify(Vga::OVERRIDE.val(u32::from(on)));
        self.pcr
            .register::<DfeOverride::Register>(lane.port, lane.base + 0x124)?
            .modify(
                DfeOverride::T0.val(u32::from(on))
                    + DfeOverride::T1.val(u32::from(on))
                    + DfeOverride::T2.val(u32::from(on))
                    + DfeOverride::T3.val(u32::from(on)),
            );
        let tap = self
            .pcr
            .register::<Tap::Register>(lane.port, lane.base + 0x164)?;
        tap.modify(Tap::UPDATE::CLEAR);
        tap.modify(Tap::UPDATE::SET);
        Ok(())
    }
    fn jim(&self, index: usize, on: bool) -> Result<(), Error> {
        let lane = self.lanes[index].ok_or(Error::Topology)?;
        self.pcr
            .register::<Jim::Register>(lane.port, lane.base + 0x1e4)?
            .modify(Jim::ENABLE.val(u32::from(on)));
        let pidac = self
            .pcr
            .register::<Pidac::Register>(lane.port, lane.base + 0x1e0)?;
        pidac.modify(Pidac::VALID::CLEAR);
        pidac.modify(Pidac::VALID::SET);
        Ok(())
    }
    fn step(&self, index: usize, step: u8) -> Result<(), Error> {
        let lane = self.lanes[index].ok_or(Error::Topology)?;
        let jim = self
            .pcr
            .register::<Jim::Register>(lane.port, lane.base + 0x1e4)?;
        jim.modify(Jim::TIMER_HIGH.val(0)); // (120 - 5) >> 8
        self.pcr
            .register::<()>(lane.port, lane.base + 0x1f0)?
            .set((115 << 24) | (1 << 16) | (1 << 8) | u32::from(step));
        self.pcr
            .register::<()>(lane.port, lane.base + 0x1ec)?
            .set(if step == 0 { 0 } else { 4 });
        jim.modify(Jim::COARSE::CLEAR + Jim::MARGIN::CLEAR + Jim::MODE::SET);
        Ok(())
    }
    fn sw_setup(&self) -> Result<(), Error> {
        for lane in self.lanes[..self.width].iter().flatten() {
            let jim = self
                .pcr
                .register::<Jim::Register>(lane.port, lane.base + 0x1e4)?;
            if jim.is_set(Jim::ENABLE) {
                return Err(Error::Owned);
            }
        }
        for lane in self.lanes[..self.width].iter().flatten() {
            self.pcr
                .register::<Dfe::Register>(lane.port, lane.base + 0x120)?
                .modify(Dfe::TAP3::CLEAR);
        }
        Ok(())
    }
    fn measure_mask(&self, mut mask: u8, delay: &mut impl DelayNs) -> Result<[u8; 4], Error> {
        if self.port.speed() != 3 || self.port.width() != self.width as u8 {
            return Err(Error::MarginLinkChanged);
        }
        mask &= (1 << self.width) - 1;
        let mut margin = [0u8; 4];
        for step in 0..32u8 {
            if mask == 0 {
                break;
            }
            // Install cleanup before any freeze/JIM update; errors cannot
            // strand adaptation frozen or jitter enabled on a different lane.
            let guard = MarginGuard { eq: self, mask };
            for lane in 0..self.width {
                if mask & (1 << lane) != 0 {
                    self.step(lane, step)?;
                    self.freeze(lane, true)?;
                }
            }
            self.port.reset_errors();
            for lane in 0..self.width {
                if mask & (1 << lane) != 0 {
                    self.jim(lane, true)?;
                }
            }
            let mut issues = 0;
            let mut changed = false;
            for _ in 0..50 {
                delay.delay_us(10);
                if self.port.r.state.read(LinkState::LTSSM) != 0x33 {
                    issues += 1;
                }
                if self.port.speed() != 3
                    || self.port.width() != self.width as u8
                    || self.port.r.eq1.read(Eq1::RECOVERY) > 40
                    || issues > 20
                {
                    changed = true;
                    break;
                }
            }
            // Disable injection BEFORE sampling counters, matching native.
            for lane in 0..self.width {
                if mask & (1 << lane) != 0 {
                    self.jim(lane, false)?;
                }
            }
            let mut errors = [0u32; 4];
            for lane in 0..self.width {
                if mask & (1 << lane) != 0 {
                    self.port.r.counter.write(Counter::LANE.val(lane as u32));
                    errors[lane] = self.port.r.counter.read(Counter::ERRORS);
                }
            }
            guard.close()?;
            if changed {
                return Err(Error::MarginLinkChanged);
            }
            for lane in 0..self.width {
                if mask & (1 << lane) != 0 {
                    margin[lane] = step;
                    if errors[lane] >= 4 {
                        mask &= !(1 << lane);
                    }
                }
            }
        }
        Ok(margin)
    }
    fn measure(&self, delay: &mut impl DelayNs) -> Result<[u8; 4], Error> {
        match self.measure_mask(15, delay) {
            Ok(margin) => Ok(margin),
            Err(Error::MarginLinkChanged) => {
                let mut margin = [0; 4];
                for lane in 0..self.width {
                    self.port.train(1, 5000, delay)?;
                    self.port.train(3, 5000, delay)?;
                    margin[lane] = self.measure_mask(1 << lane, delay)?[lane];
                }
                Ok(margin)
            }
            Err(error) => Err(error),
        }
    }
    fn update(&mut self, index: usize, margin: [u8; 4]) {
        for (lane, value) in margin[..self.width].iter().copied().enumerate() {
            self.best[lane] = best_measured(self.best[lane], index, value);
        }
    }
    fn coefficients(&self, lane: usize, pre: u8, post: u8) {
        const CM: [(usize, usize); 4] = [(0, 0), (0, 12), (0, 24), (1, 6)];
        const CP: [(usize, usize); 4] = [(0, 6), (0, 18), (1, 0), (1, 12)];
        let (reg, shift) = CM[lane];
        self.port.r.coeff[reg].modify(FieldValue::<u32, Coeff::Register>::new(
            63,
            shift,
            u32::from(pre),
        ));
        let (reg, shift) = CP[lane];
        self.port.r.coeff[reg].modify(FieldValue::<u32, Coeff::Register>::new(
            63,
            shift,
            u32::from(post),
        ));
        self.port.r.coeff[0].modify(Coeff::PCM::SET);
        self.port.r.eq1.modify(Eq1::COEFF_OVERRIDE::SET);
    }
    fn preset(&self, lane: usize, value: u8, upstream: bool) {
        if upstream {
            self.port.r.presets[lane].modify(FieldValue::<u16, ()>::new(15, 8, u16::from(value)));
        } else {
            self.port.r.coeff[0].modify(
                Coeff::PCM::CLEAR
                    + FieldValue::<u32, Coeff::Register>::new(63, lane * 6, u32::from(value)),
            );
            self.port.r.eq1.modify(Eq1::COEFF_OVERRIDE::SET);
        }
    }
    fn software(
        &mut self,
        coefficients: &[[u8; 2]; 5],
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.sw_setup()?;
        self.port.r.eq1.modify(Eq1::REMOTE_BYPASS::SET);
        for (index, [pre, post]) in coefficients.iter().copied().enumerate() {
            match self.port.train(1, 5000, delay) {
                Ok(()) => {}
                Err(Error::LinkTimeout) => continue,
                Err(e) => return Err(e),
            }
            for lane in 0..self.width {
                self.coefficients(lane, pre, post);
            }
            match self.port.train(3, 5000, delay) {
                Ok(()) => {}
                Err(Error::LinkTimeout) => continue,
                Err(e) => return Err(e),
            }
            match self.measure(delay) {
                Ok(margin) => self.update(index, margin),
                Err(Error::MarginLinkChanged | Error::LinkTimeout) => {}
                Err(e) => return Err(e),
            }
        }
        if self.best[..self.width].iter().any(Option::is_none) {
            return self.preset_search(delay);
        }
        self.port.train(1, 5000, delay)?;
        for lane in 0..self.width {
            let index = self.best[lane].ok_or(Error::NoMargin)?.0;
            self.coefficients(lane, coefficients[index][0], coefficients[index][1]);
        }
        match self.port.train(3, 5000, delay) {
            Ok(()) => Ok(()),
            Err(Error::LinkTimeout) => self.preset_search(delay),
            Err(e) => Err(e),
        }
    }
    fn preset_search(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        const PRESETS: [u8; 5] = [6, 5, 9, 4, 8];
        self.best = [None; 4];
        self.sw_setup()?;
        self.port
            .r
            .eq1
            .modify(Eq1::REMOTE_BYPASS::SET + Eq1::PHASE23_BYPASS::SET + Eq1::PHASE3_BYPASS::SET);
        let result = (|| {
            for (index, value) in PRESETS.iter().copied().enumerate() {
                for _ in 0..2 {
                    match self.port.train(1, 5000, delay) {
                        Ok(()) => {}
                        Err(Error::LinkTimeout) => continue,
                        Err(e) => return Err(e),
                    }
                    for lane in 0..self.width {
                        self.preset(lane, value, true);
                    }
                    match self.port.train(3, 5000, delay) {
                        Ok(()) => {}
                        Err(Error::LinkTimeout) => continue,
                        Err(e) => return Err(e),
                    }
                    match self.measure(delay) {
                        Ok(margin) => {
                            self.update(index, margin);
                            break;
                        }
                        Err(Error::MarginLinkChanged | Error::LinkTimeout) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            if self.best[..self.width].iter().any(Option::is_none) {
                return Err(Error::NoMargin);
            }
            self.port.train(1, 5000, delay)?;
            Ok(())
        })();
        self.port
            .r
            .eq1
            .modify(Eq1::PHASE23_BYPASS::CLEAR + Eq1::PHASE3_BYPASS::CLEAR);
        result?;
        for lane in 0..self.width {
            self.preset(
                lane,
                PRESETS[self.best[lane].ok_or(Error::NoMargin)?.0],
                false,
            );
        }
        self.port.train(3, 5000, delay)
    }
}
struct MarginGuard<'a, 'b> {
    eq: &'a Eq<'b>,
    mask: u8,
}
impl MarginGuard<'_, '_> {
    fn cleanup(&self) -> Result<(), Error> {
        let mut first = None;
        for lane in 0..self.eq.width {
            if self.mask & (1 << lane) != 0 {
                if let Err(e) = self.eq.jim(lane, false) {
                    first.get_or_insert(e);
                }
                if let Err(e) = self.eq.freeze(lane, false) {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
    fn close(mut self) -> Result<(), Error> {
        let result = self.cleanup();
        self.mask = 0;
        result
    }
}
impl Drop for MarginGuard<'_, '_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
impl HardwarePort {
    fn reset_errors(&self) {
        self.r.eq1.modify(Eq1::RESET_COUNTER::CLEAR);
        self.r.eq1.modify(Eq1::RESET_COUNTER::SET);
    }
    pub(super) fn equalize(
        &self,
        fabric: &fabric::Fabric<'_>,
        config: &Config,
        gen3_device: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let mut eq = Eq {
            port: self,
            pcr: fabric.pcr,
            lanes: [None; 4],
            width: usize::from(self.width()),
            max: usize::from(self.max_width),
            best: [None; 4],
        };
        if eq.width > eq.max || gen3_device && eq.width == 0 {
            return Err(Error::Topology);
        }
        for lane in 0..eq.max {
            eq.lanes[lane] = Some(fabric.lane(self.physical, lane as u8)?);
        }
        self.common_clock()?;
        let result = match config.ports[usize::from(self.physical)].eq {
            Equalization::Software if gen3_device => eq.software(&config.coefficients, delay),
            Equalization::Software => Ok(()),
            Equalization::Hardware => {
                self.r
                    .eq1
                    .modify(Eq1::COEFF_OVERRIDE::CLEAR + Eq1::TUPP::CLEAR);
                for (entry, values) in config.coefficients.iter().enumerate() {
                    for (n, value) in values.iter().enumerate() {
                        let field = 2 * entry + n;
                        self.r.coeff[field / 5].modify(FieldValue::<u32, Coeff::Register>::new(
                            63,
                            6 * (field % 5),
                            u32::from(*value),
                        ));
                    }
                }
                self.r.coeff[0].modify(Coeff::PCM::SET);
                self.r
                    .eq2
                    .modify(Eq2::SEARCH.val(4) + Eq2::EVAL.val(4) + Eq2::ERRORS.val(2));
                self.r.haeq.modify(Haeq::COUNT.val(4) + Haeq::MAC::CLEAR);
                if gen3_device {
                    self.train(3, 5000, delay)
                } else {
                    Ok(())
                }
            }
            Equalization::Static => {
                self.r.eq1.modify(Eq1::REMOTE_BYPASS::SET);
                for lane in 0..eq.max {
                    let index = usize::from(fabric.lane_index(self.physical, lane as u8)?);
                    eq.coefficients(
                        lane,
                        config.static_coefficients[index][0],
                        config.static_coefficients[index][1],
                    );
                }
                if gen3_device {
                    self.train(3, 5000, delay)
                } else {
                    Ok(())
                }
            }
        };
        // Only explicitly selected status is acknowledged, never an RW1C echo.
        self.r.correctable.set((1 << 7) | (1 << 6) | 1);
        self.r.dsta.write(Dsta::CORRECTABLE::SET);
        self.r.lane_error.set(u32::MAX);
        self.reset_errors();
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_margin_does_not_fabricate_a_winner_and_ties_keep_first_measured_choice() {
        assert_eq!(best_measured(None, 0, 0), None);
        let measured = best_measured(None, 3, 7);
        assert_eq!(measured, Some((3, 7)));
        assert_eq!(best_measured(measured, 4, 0), measured);
        assert_eq!(best_measured(measured, 4, 7), measured);
        assert_eq!(best_measured(measured, 4, 8), Some((4, 8)));
    }
}
