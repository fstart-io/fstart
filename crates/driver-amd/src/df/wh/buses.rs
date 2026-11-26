// SPDX-License-Identifier: MIT OR Apache-2.0
//! DFv4.5 PCI bus reallocation: the vendor's PEI planner (`FabricRcInitWh`,
//! Dell SilVenicePei `0xf3e930`; GIGABYTE `0x709a42e4`, which lacks the
//! shrink; 2P_COMPLETE §2.10), as a pure function. [`super::program_buses`]
//! writes its result.
//!
//! The vendor runs it only from an NV target its DXE stored before a warm
//! reset. Applied before NBIO and MPIO (which address the IODs by bus) it
//! needs no reset: the MMIO and I/O maps route by fabric ID and stay valid
//! (`RcMgrWh` `0xf20e90` programs MMIO, the B0 gap patch and I/O before the
//! buses too).
//!
//! Buses are global numbers, `segment * 256 + bus`; a root bridge never
//! spans two segments ([`super::Node::set_config_route`] has one segment
//! field per pair).
//!
//! 1. Every root bridge's target is its request, or its live count where it
//!    asks for none (`0`). All equal to the live map: nothing to do.
//! 2. More buses than the ECAM decodes (`capacity`, `1 << MSR
//!    C001_0058[5:2]`): root bridges are shrunk from the last socket's last
//!    one backwards, none below [`MIN_BUSES`]; still too many is an error
//!    (the ABL's map stays).
//! 3. The FCH root bridge gets pair 0 and buses `0..target`.
//! 4. The others follow in the input order (socket, die, the APOB's `9/0x2C`
//!    order on the die), each from the last one's limit + 1. The last of a
//!    die takes the rest of its segment when it is the system's last, has
//!    no target, would cross the segment, or the next die's total does not
//!    fit behind it ("Assign remaining buses"); so a die never straddles two
//!    segments.

use heapless::Vec;

use super::CONFIG_PAIRS;
use crate::df::BusRange;

/// The fewest buses the shrink leaves a root bridge.
pub const MIN_BUSES: u16 = 0x10;
/// Most buses: 8 segments of 256 (MSR `C001_0058` BusRange 11).
pub const MAX_BUSES: u16 = 8 * 256;

/// A root bridge as the planner takes it, in the vendor's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub socket: u8,
    pub die: u8,
    /// Physical root bridge on the IOD (its IOS instance minus `Ios0`).
    pub rb: u8,
    pub fabric_id: u16,
    /// Live buses (global numbers), if the ABL routed any.
    pub live: Option<(u16, u16)>,
    /// Buses wanted, `0` to keep the live count (the vendor's
    /// `PciBusNumber[socket][rb]`).
    pub target: u16,
}

impl Request {
    /// The live bus count.
    pub fn live_count(&self) -> u16 {
        self.live.map_or(0, |(base, limit)| limit - base + 1)
    }
}

/// What a root bridge gets: CfgAddressMap pair `pair`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assign {
    pub pair: u8,
    pub socket: u8,
    pub die: u8,
    pub rb: u8,
    pub fabric_id: u16,
    pub segment: u8,
    /// Buses within `segment`.
    pub buses: BusRange,
}

impl Assign {
    /// The global number of the first bus (`F0xC04`, `segment << 8 | bus`).
    pub const fn global_base(&self) -> u16 {
        (self.segment as u16) << 8 | self.buses.base as u16
    }
}

/// Why no map was planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No root bridges, more than the 16 pairs, or no FCH root bridge.
    Count,
    /// More buses than the ECAM decodes, even with every root bridge shrunk.
    Capacity,
    /// A root bridge would span two segments.
    Segment,
}

impl Error {
    /// For the log.
    pub const fn why(self) -> &'static str {
        match self {
            Self::Count => "DFv4.5 bus plan: root bridge count",
            Self::Capacity => "DFv4.5 bus plan: more buses than the ECAM decodes",
            Self::Segment => "DFv4.5 bus plan: a root bridge across segments",
        }
    }
}

/// A plan: one pair per root bridge, pair order.
pub type Plan = Vec<Assign, { CONFIG_PAIRS as usize }>;

/// Plan the buses of `rbs` (vendor order; `rbs[fch]` is the FCH root
/// bridge of socket 0's die 0) in an ECAM of `capacity` buses. `Ok(None)`:
/// the live map already is the target.
pub fn plan(rbs: &[Request], fch: usize, capacity: u16) -> Result<Option<Plan>, Error> {
    if rbs.is_empty() || rbs.len() > usize::from(CONFIG_PAIRS) || fch >= rbs.len() {
        return Err(Error::Count);
    }
    let capacity = u32::from(capacity.min(MAX_BUSES));
    let mut targets: Vec<u32, { CONFIG_PAIRS as usize }> = rbs
        .iter()
        .map(|rb| {
            u32::from(if rb.target == 0 {
                rb.live_count()
            } else {
                rb.target
            })
        })
        .collect();
    let unchanged = rbs
        .iter()
        .zip(&targets)
        .all(|(rb, &target)| rb.live.is_some() && u32::from(rb.live_count()) == target);
    // A live map the ECAM decodes whole is kept.
    let decoded = rbs.iter().all(|rb| {
        rb.live
            .is_some_and(|(_, limit)| u32::from(limit) < capacity)
    });
    if unchanged && decoded {
        return Ok(None);
    }
    // Shrink from the last root bridge backwards.
    let mut total: u32 = targets.iter().sum();
    for target in targets.iter_mut().rev() {
        if total <= capacity {
            break;
        }
        let spare = target.saturating_sub(u32::from(MIN_BUSES));
        let cut = spare.min(total - capacity);
        *target -= cut;
        total -= cut;
    }
    if total > capacity {
        return Err(Error::Capacity);
    }
    let die = |i: usize| (rbs[i].socket, rbs[i].die);
    let order: Vec<usize, { CONFIG_PAIRS as usize }> =
        (0..rbs.len()).filter(|&i| i != fch).collect();
    let mut plan = Plan::new();
    let mut push = |i: usize, base: u32, limit: u32| -> Result<(), Error> {
        if base > limit || limit >= capacity {
            return Err(Error::Capacity);
        }
        if base >> 8 != limit >> 8 {
            return Err(Error::Segment);
        }
        let rb = &rbs[i];
        let _ = plan.push(Assign {
            pair: plan.len() as u8,
            socket: rb.socket,
            die: rb.die,
            rb: rb.rb,
            fabric_id: rb.fabric_id,
            segment: (base >> 8) as u8,
            buses: BusRange {
                base: base as u8,
                limit: limit as u8,
            },
        });
        Ok(())
    };
    // The FCH root bridge first, from bus 0.
    push(fch, 0, targets[fch].max(1) - 1)?;
    let mut last = targets[fch].max(1) - 1;
    for (k, &i) in order.iter().enumerate() {
        let base = last + 1;
        let target = targets[i];
        let want = base - 1 + target;
        let last_of_die = order.get(k + 1).is_none_or(|&next| die(next) != die(i));
        let limit = if !last_of_die {
            want
        } else {
            let segment_end = (base & !0xff) | 0xff;
            // The next die's root bridges, FCH root bridge aside.
            let next = order.get(k + 1).map(|&next| die(next));
            let next_total: u32 = order[k + 1..]
                .iter()
                .filter(|&&j| Some(die(j)) == next)
                .map(|&j| targets[j])
                .sum();
            if next.is_none()
                || target == 0
                || want > segment_end
                || next_total > segment_end - want
            {
                segment_end
            } else {
                want
            }
        };
        push(i, base, limit)?;
        last = limit;
    }
    Ok(Some(plan))
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec as StdVec;

    /// `sockets` × 2 dies × `per_iod` root bridges with live maps of `live`
    /// buses each from bus 0 (globally), FCH first.
    fn system(sockets: u8, per_iod: u8, live: u16) -> StdVec<Request> {
        let mut next = 0u16;
        let mut rbs = StdVec::new();
        for socket in 0..sockets {
            for die in 0..2 {
                for rb in 0..per_iod {
                    rbs.push(Request {
                        socket,
                        die,
                        rb,
                        fabric_id: u16::from(socket) << 7
                            | u16::from(die) << 6
                            | (0x26 + u16::from(rb)),
                        live: Some((next, next + live - 1)),
                        target: 0,
                    });
                    next += live;
                }
            }
        }
        rbs
    }

    fn global(plan: &Plan) -> StdVec<(u16, u16)> {
        plan.iter()
            .map(|a| {
                let seg = u16::from(a.segment) << 8;
                (
                    seg | u16::from(a.buses.base),
                    seg | u16::from(a.buses.limit),
                )
            })
            .collect()
    }

    #[test]
    fn the_live_map_is_kept_when_it_is_the_target() {
        let rbs = system(2, 3, 21);
        assert_eq!(plan(&rbs, 0, 256), Ok(None));
        // The same map with an explicit equal request.
        let mut asked = rbs.clone();
        asked[4].target = 21;
        assert_eq!(plan(&asked, 0, 256), Ok(None));
    }

    /// 1P, 6 root bridges, one asks for 64 buses: the others keep theirs,
    /// the last takes the rest of segment 0.
    #[test]
    fn a_board_request_on_1p() {
        let mut rbs = system(1, 3, 42);
        rbs[2].target = 64;
        let plan = plan(&rbs, 0, 256).unwrap().unwrap();
        assert_eq!(
            global(&plan),
            [
                (0, 41),
                (42, 83),
                (84, 147),
                (148, 189),
                (190, 231),
                (232, 255)
            ]
        );
        assert!(plan.iter().enumerate().all(|(k, a)| a.pair == k as u8));
        assert_eq!(plan[2].fabric_id, 0x28);
    }

    /// A Dell-like 2P map over two segments (each root bridge 42 buses, 12
    /// of them: 504 buses) in a one-segment ECAM: shrunk from the end to
    /// fit 256, all in segment 0.
    #[test]
    fn a_two_segment_map_fits_segment_0() {
        let rbs = system(2, 3, 42);
        assert!(rbs.last().unwrap().live.unwrap().1 > 255);
        let plan = plan(&rbs, 0, 256).unwrap().unwrap();
        let buses = global(&plan);
        assert!(plan.iter().all(|a| a.segment == 0));
        assert_eq!(buses.last().unwrap().1, 255);
        let counts: StdVec<u16> = buses.iter().map(|(b, l)| l - b + 1).collect();
        assert!(counts.iter().all(|&c| c >= MIN_BUSES));
        // The first root bridges keep their 42, the last ones give way.
        assert_eq!(&counts[..2], [42; 2]);
        assert_eq!(counts[2], 28);
        assert_eq!(counts.iter().map(|&c| u32::from(c)).sum::<u32>(), 256);
        for w in buses.windows(2) {
            assert_eq!(w[0].1 + 1, w[1].0);
        }
        // With two segments decoded, the live map stays as the ABL left it.
        assert_eq!(plan_two(&rbs), None);
    }

    fn plan_two(rbs: &[Request]) -> Option<Plan> {
        plan(rbs, 0, 512).unwrap()
    }

    /// Dies never straddle a segment: the last root bridge of a die whose
    /// successor does not fit takes the rest of the segment.
    #[test]
    fn dies_start_a_new_segment_when_they_do_not_fit() {
        let mut rbs = system(2, 3, 30);
        for rb in rbs.iter_mut() {
            rb.target = 60;
        }
        let plan = plan(&rbs, 0, 1024).unwrap().unwrap();
        let buses = global(&plan);
        // Die (0,0): 0..60, 60..120, 120..180; die (0,1) needs 180 > 75
        // left: (0,0) rb 2 ends at 255.
        assert_eq!(buses[2], (120, 255));
        assert_eq!(buses[3], (256, 315));
        assert_eq!(buses[5], (376, 511));
        assert_eq!(buses[11], (888, 1023));
        assert!(plan.iter().all(|a| a.buses.base <= a.buses.limit));
        assert_eq!(plan[3].segment, 1);
        assert_eq!(plan[3].global_base(), 0x100);
    }

    /// Too many buses even at the shrink floor; no FCH; too many pairs.
    #[test]
    fn overflow_and_bad_inputs() {
        let mut rbs = system(2, 3, 21);
        for rb in rbs.iter_mut() {
            rb.target = 0x100;
        }
        // 12 × 16 = 192 fits 256 after the shrink...
        assert!(plan(&rbs, 0, 256).unwrap().is_some());
        // ...not 128.
        assert_eq!(plan(&rbs, 0, 128), Err(Error::Capacity));
        assert_eq!(plan(&[], 0, 256), Err(Error::Count));
        assert_eq!(plan(&rbs, 12, 256), Err(Error::Count));
        let many: StdVec<_> = system(2, 3, 10).into_iter().cycle().take(17).collect();
        assert_eq!(plan(&many, 0, 256), Err(Error::Count));
        // The FCH die's root bridges alone cross segment 0.
        let mut wide = system(1, 3, 21);
        for (rb, target) in wide.iter_mut().zip([200, 100, 100]) {
            rb.target = target;
        }
        assert_eq!(plan(&wide, 0, 1024), Err(Error::Segment));
    }

    /// The FCH root bridge comes first wherever it is in the order.
    #[test]
    fn the_fch_root_bridge_gets_bus_0() {
        let mut rbs = system(1, 3, 42);
        rbs.swap(0, 1);
        rbs[5].target = 30;
        let plan = plan(&rbs, 1, 256).unwrap().unwrap();
        assert_eq!((plan[0].rb, plan[0].buses.base), (0, 0));
        assert_eq!(plan[1].rb, 1);
    }
}
