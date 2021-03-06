use extra::sort::quick_sort;
use extra::smallintmap::SmallIntMap;
use std::{vec, uint, iterator};
use linearscan::{KindHelper, RegisterHelper, GroupHelper};
use linearscan::graph::{Graph, Interval,
                        IntervalId, InstrId, StackId, BlockId,
                        UseAny, UseRegister, UseFixed,
                        Value, RegisterVal, StackVal};
use linearscan::flatten::Flatten;
use linearscan::liveness::Liveness;
use linearscan::gap::GapResolver;

pub struct AllocatorResult {
  spill_count: ~[uint]
}

struct GroupResult {
  spill_count: uint
}

struct AllocatorState<G, R> {
  group: ~G,
  register_count: uint,
  spill_count: uint,
  spills: ~[Value<G, R>],
  unhandled: ~[IntervalId],
  active: ~[IntervalId],
  inactive: ~[IntervalId]
}

pub trait Allocator {
  // Prepare for allocation
  fn prepare(&mut self);

  // Allocate registers
  fn allocate(&mut self) -> Result<AllocatorResult, ~str>;
}

enum SplitConf {
  Between(InstrId, InstrId),
  At(InstrId)
}

trait AllocatorHelper<G: GroupHelper<R>, R: RegisterHelper<G> > {
  // Walk unhandled intervals in the order of increasing starting point
  fn walk_intervals(&mut self, group: &G) -> Result<GroupResult, ~str>;
  // Try allocating free register
  fn allocate_free_reg<'r>(&'r mut self,
                           current: IntervalId,
                           state: &'r mut AllocatorState<G, R>) -> bool;
  // Allocate blocked register and spill others, or spill interval itself
  fn allocate_blocked_reg<'r>(&'r mut self,
                              current: IntervalId,
                              state: &'r mut AllocatorState<G, R>)
      -> Result<(), ~str>;
  // Add movements on block edges
  fn resolve_data_flow(&mut self, list: &[BlockId]);

  // Build live ranges for each interval
  fn build_ranges(&mut self, blocks: &[BlockId]) -> Result<(), ~str>;

  // Split intervals with fixed uses
  fn split_fixed(&mut self);

  //
  // Helpers
  //

  // Sort unhandled list (after insertion)
  fn sort_unhandled<'r>(&'r mut self, state: &'r mut AllocatorState<G, R>);

  // Get register hint if present
  fn get_hint(&mut self, current: IntervalId) -> Option<R>;

  // Split interval at some optimal position and add split child to unhandled
  fn split<'r>(&'r mut self,
               current: IntervalId,
               conf: SplitConf,
               state: &'r mut AllocatorState<G, R>) -> IntervalId;

  // Split and spill all intervals intersecting with current
  fn split_and_spill<'r>(&'r mut self,
                         current: IntervalId,
                         state: &'r mut AllocatorState<G, R>);

  // Iterate through all active intervals
  fn iter_active<'r>(&'r self, state: &'r AllocatorState<G, R>)
      -> iterator::Map<'r,
                       &IntervalId,
                       (&IntervalId, &R),
                       vec::VecIterator<IntervalId> >;

  // Iterate through all inactive intervals that are intersecting with current
  fn iter_intersecting<'r>(&'r self,
                           current: IntervalId,
                           state: &'r AllocatorState<G, R>)
      -> iterator::FilterMap<'r,
                             &IntervalId,
                             (&IntervalId, &R, InstrId),
                             vec::VecIterator<IntervalId> >;

  // Verify allocation results
  fn verify(&self);
}

impl<G: GroupHelper<R>,
     R: RegisterHelper<G>,
     K: KindHelper<G, R> > Allocator for Graph<K, G, R> {
  fn prepare(&mut self) {
    if self.prepared {
      return;
    }

    // Get flat list of blocks
    self.flatten();

    // Build live_in/live_out
    self.liveness_analysis();

    self.prepared = true;
  }

  fn allocate(&mut self) -> Result<AllocatorResult, ~str> {
    self.prepare();

    // Create physical fixed intervals
    let groups: ~[G] = GroupHelper::groups();
    for group in groups.iter() {
      self.physical.insert(group.to_uint(), ~SmallIntMap::new());
      let regs = group.registers();
      for reg in regs.iter() {
        let interval = Interval::<G, R>::new::<K>(self, group.clone());
        self.get_mut_interval(&interval).value = RegisterVal(reg.clone());
        self.get_mut_interval(&interval).fixed = true;
        self.physical.find_mut(&group.to_uint()).unwrap().insert(reg.to_uint(),
                                                                 interval);
      }
    }

    let list = self.get_block_list();

    // Create live ranges
    match self.build_ranges(list) {
      Ok(_) => {
        let mut results = ~[];
        // In each register group
        for group in groups.iter() {
          // Walk intervals!
          match self.walk_intervals(group) {
            Ok(res) => {
              results.push(res);
            },
            Err(reason) => { return Err(reason); }
          }
        }

        // Add moves between blocks
        self.resolve_data_flow(list);

        // Resolve parallel moves
        self.resolve_gaps();

        // Verify correctness of allocation
        self.verify();

        // Map results from each group to a general result
        return Ok(AllocatorResult {
          spill_count: do results.map() |result| {
            result.spill_count
          }
        });
      },
      Err(reason) => { return Err(reason); }
    };
  }
}

impl<G: GroupHelper<R>,
     R: RegisterHelper<G>,
     K: KindHelper<G, R> > AllocatorHelper<G, R> for Graph<K, G, R> {
  fn walk_intervals(&mut self,
                    group: &G) -> Result<GroupResult, ~str> {
    // Initialize allocator state
    let reg_count = group.registers().len();
    let mut state = ~AllocatorState {
      group: ~group.clone(),
      register_count: reg_count,
      spill_count: 0,
      spills: ~[],
      unhandled: ~[],
      active: ~[],
      inactive: ~[]
    };

    // We'll work with intervals that contain any ranges
    for (_, interval) in self.intervals.iter() {
      if &interval.value.group() == state.group && interval.ranges.len() > 0 {
        if interval.fixed {
          // Push all physical registers to active
          state.active.push(interval.id);
        } else {
          // And everything else to unhandled
          state.unhandled.push(interval.id);
        }
      }
    }
    self.sort_unhandled(state);

    while state.unhandled.len() > 0 {
      let current = state.unhandled.shift();
      let position = self.get_interval(&current).start();

      // active => inactive or handled
      let mut handled = ~[];
      do state.active.retain |id| {
        if self.get_interval(id).covers(position) {
          true
        } else {
          if position <= self.get_interval(id).end() {
            state.inactive.push(*id);
          }
          handled.push(self.get_interval(id).value.clone());
          false
        }
      };

      // inactive => active or handled
      do state.inactive.retain |id| {
        if self.get_interval(id).covers(position) {
          state.active.push(*id);
          handled.push(self.get_interval(id).value.clone());
          false
        } else {
          position < self.get_interval(id).end()
        }
      };

      // Return handled spills
      for v in handled.iter() {
        state.to_handled(v)
      }

      // Skip non-virtual intervals
      if self.get_interval(&current).value.is_virtual() {
        // Allocate free register
        if !self.allocate_free_reg(current, state) {
          // Or spill some active register
          match self.allocate_blocked_reg(current, state) {
            Ok(_) => (),
            Err(err) => {
              return Err(err);
            }
          }
        }
      }

      // Push register interval to active
      match self.get_interval(&current).value {
        RegisterVal(_) => state.active.push(current),
        _ => ()
      }
    }

    return Ok(GroupResult { spill_count: state.spill_count });
  }

  fn allocate_free_reg<'r>(&'r mut self,
                           current: IntervalId,
                           state: &'r mut AllocatorState<G, R>) -> bool {
    let mut free_pos = vec::from_elem(state.register_count, uint::max_value);
    let hint = self.get_hint(current);

    // All active intervals use registers
    for (_, reg) in self.iter_active(state) {
      free_pos[reg.to_uint()] = 0;
    }

    // All inactive registers will eventually use registers
    for (_, reg, pos) in self.iter_intersecting(current, state) {
      if free_pos[reg.to_uint()] > pos.to_uint() {
        free_pos[reg.to_uint()] = pos.to_uint();
      }
    }

    // Choose register with maximum free_pos
    let mut reg = 0;
    let mut max_pos = InstrId(0);
    match self.get_interval(&current).next_fixed_use(InstrId(0)) {
      // Intervals with fixed use should have specific register
      Some(u) => {
        match u.kind {
          UseFixed(r) => {
            reg = r.to_uint();
            max_pos = InstrId(free_pos[reg]);
          },
          _ => fail!("Unexpected use kind")
        }
      },

      // Other intervals should prefer register that's free for a longer time
      None => {
        // Prefer hinted register
        match hint {
          Some(hint) => for (i, &pos) in free_pos.iter().enumerate() {
            if pos > max_pos.to_uint() ||
               hint.to_uint() == i && pos == max_pos.to_uint() {
              max_pos = InstrId(pos);
              reg = i;
            }
          },
          None => for (i, &pos) in free_pos.iter().enumerate() {
            if pos > max_pos.to_uint() {
              max_pos = InstrId(pos);
              reg = i;
            }
          }
        }
      }
    }

    if max_pos.to_uint() == 0 {
      // All registers are blocked - failure
      return false;
    }

    let start = self.get_interval(&current).start();
    let end = self.get_interval(&current).end();
    if max_pos >= end {
      // Register is available for whole current's lifetime
    } else if start.next() >= max_pos {
      // Allocation is impossible
      return false;
    } else {
      // Register is available for some part of current's lifetime
      assert!(max_pos < end);

      let mut split_pos = self.optimal_split_pos(state.group, start, max_pos);
      if split_pos == max_pos.prev() && self.clobbers(state.group, &max_pos) {
        // Splitting right before `call` instruction is pointless,
        // unless we have a register use at that instruction,
        // try spilling current instead.
        match self.get_interval(&current).next_use(max_pos) {
          Some(ref u) if u.pos == max_pos => {
            split_pos = max_pos;
          },
          _ => {
            return false;
          }
        }
      }
      let child = self.split(current, At(split_pos), state);

      // Fast case, spill child if there're no register uses after split
      match self.get_interval(&child).next_use(InstrId(0)) {
        None => {
          self.get_mut_interval(&child).value = state.get_spill();
        },
        _ => ()
      }
    }

    // Give current a register
    self.get_mut_interval(&current).value =
        RegisterVal(RegisterHelper::from_uint::<G, R>(state.group, reg));

    return true;
  }

  fn allocate_blocked_reg<'r>(&'r mut self,
                              current: IntervalId,
                              state: &'r mut AllocatorState<G, R>)
      -> Result<(), ~str> {
    let mut use_pos = vec::from_elem(state.register_count, uint::max_value);
    let mut block_pos = vec::from_elem(state.register_count, uint::max_value);
    let start = self.get_interval(&current).start();
    let hint = self.get_hint(current);

    // Populate use_pos from every non-fixed interval
    for (id, reg) in self.iter_active(state) {
      let interval = self.get_interval(id);
      if !interval.fixed {
        let int_reg = reg.to_uint();
        match interval.next_use(start) {
          Some(u) => if use_pos[int_reg] > u.pos.to_uint() {
            use_pos[int_reg] = u.pos.to_uint();
          },
          None => ()
        }
      }
    }
    for (id, reg, _) in self.iter_intersecting(current, state) {
      let interval = self.get_interval(id);
      if !interval.fixed {
        let int_reg = reg.to_uint();
        match interval.next_use(start) {
          Some(u) => if use_pos[int_reg] > u.pos.to_uint() {
            use_pos[int_reg] = u.pos.to_uint();
          },
          None => ()
        }
      }
    }

    // Populate block_pos from every fixed interval
    for (id, reg) in self.iter_active(state) {
      if self.get_interval(id).fixed {
        let int_reg = reg.to_uint();
        block_pos[int_reg] = 0;
        use_pos[int_reg] = 0;
      }
    }
    for (id, reg, pos) in self.iter_intersecting(current, state) {
      if self.get_interval(id).fixed {
        let int_reg = reg.to_uint();
        let int_pos = pos.to_uint();
        block_pos[int_reg] = int_pos;
        if use_pos[int_reg] > int_pos {
          use_pos[int_reg] = int_pos;
        }
      }
    }

    // Find register with the farest use
    let mut reg = 0;
    let mut max_pos = 0;
    match self.get_interval(&current).next_fixed_use(InstrId(0)) {
      // Intervals with fixed use should have specific register
      Some(u) => {
        match u.kind {
          UseFixed(r) => {
            reg = r.to_uint();
            max_pos = use_pos[reg];
          },
          _ => fail!("Unexpected use kind")
        }
      },

      // Other intervals should prefer register that isn't used for longer time
      None => {
        // Prefer hinted register
        match hint {
          Some(hint) => for (i, &pos) in use_pos.iter().enumerate() {
            if pos > max_pos || hint.to_uint() == i && pos == max_pos {
              max_pos = pos;
              reg = i;
            }
          },
          None => for (i, &pos) in use_pos.iter().enumerate() {
            if pos > max_pos {
              max_pos = pos;
              reg = i;
            }
          }
        }
      }
    }

    let first_use = self.get_interval(&current).next_use(InstrId(0));
    match first_use {
      Some(u) => {
        if max_pos < u.pos.to_uint() {
          if u.pos == start {
            return Err(~"Incorrect input, allocation impossible");
          }

          // Spill current itself
          self.get_mut_interval(&current).value = state.get_spill();

          // And split before first register use
          self.split(current, Between(start, u.pos), state);
        } else {
          // Assign register to current
          self.get_mut_interval(&current).value =
              RegisterVal(RegisterHelper::from_uint(state.group, reg));

          // If blocked somewhere before end by fixed interval
          if block_pos[reg] <= self.get_interval(&current).end().to_uint() {
            // Split before this position
            self.split(current, Between(start, InstrId(block_pos[reg])), state);
          }

          // Split and spill, active and intersecting inactive
          self.split_and_spill(current, state);
        }
      },
      None => {
        // Spill current, it has no uses
        self.get_mut_interval(&current).value = state.get_spill();
      }
    }
    return Ok(());
  }

  fn iter_active<'r>(&'r self, state: &'r AllocatorState<G, R>)
      -> iterator::Map<'r,
                       &IntervalId,
                       (&IntervalId, &R),
                       vec::VecIterator<IntervalId> > {
    state.active.iter().map(|id| {
      match self.get_interval(id).value {
        RegisterVal(ref reg) => (id, reg),
        _ => fail!("Expected register in active")
      }
    })
  }

  // Iterate through all inactive intervals that are intersecting with current
  fn iter_intersecting<'r>(&'r self,
                           current: IntervalId,
                           state: &'r AllocatorState<G, R>)
      -> iterator::FilterMap<'r,
                             &IntervalId,
                             (&IntervalId, &R, InstrId),
                             vec::VecIterator<IntervalId> > {
    state.inactive.iter().filter_map(|id| {
      match self.get_intersection(id, &current) {
        Some(pos) => match self.get_interval(id).value {
          RegisterVal(ref reg) => Some((id, reg, pos)),
          _ => fail!("Expected register in inactive")
        },
        None => None
      }
    })
  }

  fn sort_unhandled<'r>(&'r mut self, state: &'r mut AllocatorState<G, R>) {
    // TODO(indutny) do sorted inserts and don't call this on every insertion,
    // it is really expensive!

    // Sort intervals in the order of increasing start position
    do quick_sort(state.unhandled) |left, right| {
      let lstart = self.get_interval(left).start();
      let rstart = self.get_interval(right).start();

      lstart <= rstart
    };
  }

  fn get_hint(&mut self, current: IntervalId) -> Option<R> {
    match self.get_interval(&current).hint {
      Some(ref id) => match self.get_interval(id).value {
        RegisterVal(ref r) => {
          assert!(r.group() == self.get_interval(&current).value.group());
          Some(r.clone())
        },
        _ => None
      },
      None => None
    }
  }

  fn split<'r>(&'r mut self,
               current: IntervalId,
               conf: SplitConf,
               state: &'r mut AllocatorState<G, R>) -> IntervalId {
    let split_pos = match conf {
      Between(start, end) => self.optimal_split_pos(state.group, start, end),
      At(pos) => pos
    };

    let res = self.split_at(&current, split_pos);
    state.unhandled.push(res);
    self.sort_unhandled(state);
    return res;
  }

  fn split_and_spill<'r>(&'r mut self,
                         current: IntervalId,
                         state: &'r mut AllocatorState<G, R>) {
    let reg = match self.get_interval(&current).value {
      RegisterVal(ref r) => r.clone(),
      _ => fail!("Expected register value")
    };
    let start = self.get_interval(&current).start();

    // Filter out intersecting intervals
    let mut to_split = ~[];
    for (id, _reg) in self.iter_active(state) {
      if _reg == &reg {
        to_split.push(id);
      }
    }
    for (id, _reg, _) in self.iter_intersecting(current, state) {
      if _reg == &reg {
        to_split.push(id);
      }
    }

    // Split and spill!
    for id in to_split.iter() {
      // Spill before or at start of `current`
      let spill_pos = if self.clobbers(state.group, &start) ||
                         self.is_gap(&start) {
        start
      } else {
        start.prev()
      };
      let last_use = match self.get_interval(id).last_use(spill_pos) {
        Some(u) => u.pos,
        None => self.get_interval(id).start()
      };

      let spill_child = self.split(*id, Between(last_use, spill_pos), state);
      self.get_mut_interval(&spill_child).value = state.get_spill();

      // Split before next register use position
      match self.get_interval(&spill_child).next_use(spill_pos) {
        Some(u) => {
          self.split(*id, Between(spill_pos, u.pos), state);
        },

        // Let it be spilled for the rest of lifetime
        None() => ()
      }
    };
  }

  fn resolve_data_flow(&mut self, list: &[BlockId]) {
    for block_id in list.iter() {
      let block_end = self.get_block(block_id).end().prev();
      let successors = self.get_block(block_id).successors.clone();
      for succ_id in successors.iter() {
        let succ_start = self.get_block(succ_id).start().clone();
        let live_in = self.get_block(succ_id).live_in.clone();

        for interval in live_in.iter() {
          let interval_id = IntervalId(interval);
          let parent = match self.get_interval(&interval_id).parent {
            Some(p) => p,
            None => interval_id
          };

          let from = self.child_at(&parent, block_end)
                         .expect("Interval should exist at pred end");
          let to = self.child_at(&parent, succ_start)
                       .expect("Interval should exist at succ start");
          if from != to {
            let gap_pos = if successors.len() == 2 {
              succ_start
            } else {
              block_end
            };
            self.get_mut_gap(&gap_pos).add_move(&from, &to);
          }
        }
      }
    }
  }

  fn build_ranges(&mut self, blocks: &[BlockId])
      -> Result<(), ~str> {
    let physical = self.physical.clone();
    for block_id in blocks.rev_iter() {
      let instructions = self.get_block(block_id).instructions.clone();
      let live_out = self.get_block(block_id).live_out.clone();
      let block_from = self.get_block(block_id).start();
      let block_to = self.get_block(block_id).end();

      // Assume that each live_out interval lives for the whole time of block
      // NOTE: we'll shorten it later if definition of this interval appears to
      // be in this block
      for &int_id in live_out.iter() {
        self.get_mut_interval(&IntervalId(int_id))
            .add_range(block_from, block_to);
      }

      for &instr_id in instructions.rev_iter() {
        let instr = self.get_instr(&instr_id).clone();

        // Call instructions should swap out all used registers into stack slots
        let groups: ~[G] = GroupHelper::groups();
        for group in groups.iter() {
          self.physical.insert(group.to_uint(), ~SmallIntMap::new());
          if instr.kind.clobbers(group) {
            let regs = group.registers();
            for reg in regs.iter() {
              self.get_mut_interval(physical.get(&group.to_uint())
                  .get(&reg.to_uint()))
                  .add_range(instr_id, instr_id.next());
            }
          }
        }

        // Process output
        match instr.output {
          Some(output) => {
            // Call instructions are defining their value after the call
            let group = self.get_interval(&output).value.group();
            let pos = if instr.kind.clobbers(&group) {
              instr_id.next()
            } else {
              instr_id
            };

            if self.get_interval(&output).ranges.len() != 0  {
              // Shorten range if output outlives block, or is used anywhere
              self.get_mut_interval(&output).first_range().start = pos;
            } else {
              // Add short range otherwise
              self.get_mut_interval(&output).add_range(pos, pos.next());
            }
            let out_kind = instr.kind.result_kind().unwrap();
            self.get_mut_interval(&output).add_use(out_kind, pos);
          },
          None => ()
        }

        // Process temporary
        for tmp in instr.temporary.iter() {
          let group = self.get_interval(tmp).value.group();
          if instr.kind.clobbers(&group) {
            return Err(~"Call instruction can't have temporary registers");
          }
          self.get_mut_interval(tmp).add_range(instr_id, instr_id.next());
          self.get_mut_interval(tmp).add_use(group.use_reg(), instr_id);
        }

        // Process inputs
        for (i, input_instr) in instr.inputs.iter().enumerate() {
          let input = self.get_output(input_instr);
          if !self.get_interval(&input).covers(instr_id) {
            self.get_mut_interval(&input).add_range(block_from, instr_id);
          }
          let kind = instr.kind.use_kind(i);
          self.get_mut_interval(&input).add_use(kind, instr_id);
        }
      }
    }

    // Now split all intervals with fixed uses
    self.split_fixed();

    return Ok(());
  }

  fn split_fixed(&mut self) {
    let mut list = ~[];
    for (_, interval) in self.intervals.iter() {
      if interval.uses.any(|u| { u.kind.is_fixed() }) {
        list.push(interval.id);
      }
    }
    for id in list.iter() {
      let cur = *id;

      let mut uses = self.get_interval(id).uses.clone();
      do uses.retain |u| {
        u.kind.is_fixed()
      };

      let mut i = 0;
      while i < uses.len() - 1 {
        // Split between each pair of uses
        let split_pos = self.optimal_split_pos(&uses[i].kind.group(),
                                               uses[i].pos,
                                               uses[i + 1].pos);
        self.split_at(&cur, split_pos);

        i += 1;
      }
    }
  }

  #[cfg(test)]
  fn verify(&self) {
    for (_, interval) in self.intervals.iter() {
      if interval.ranges.len() > 0 {
        // Every interval should have a non-virtual value
        assert!(!interval.value.is_virtual());

        // Each use should receive the same type of input as it has requested
        for u in interval.uses.iter() {
          // Allocated groups should not differ from specified
          assert!(u.kind.group() == interval.value.group());
          match u.kind {
            // Any use - no restrictions
            UseAny(_) => (),
            UseRegister(_) => match interval.value {
              RegisterVal(_) => (), // ok
              _ => fail!("Register expected")
            },
            UseFixed(ref r0) => match interval.value {
              RegisterVal(ref r1) if r0 == r1 => (), // ok
              _ => fail!("Expected fixed register")
            }
          }
        }
      }
    }
  }
  #[cfg(not(test))]
  fn verify(&self) {
    // Production mode, no verification
  }
}

impl<G: GroupHelper<R>, R: RegisterHelper<G> > AllocatorState<G, R> {
  fn get_spill(&mut self) -> Value<G, R> {
    return if self.spills.len() > 0 {
      self.spills.shift()
    } else {
      let slot = self.spill_count;
      self.spill_count += 1;
      StackVal(*self.group.clone(), StackId(slot))
    }
  }

  fn to_handled(&mut self, value: &Value<G, R>) {
    match value {
      &StackVal(ref group, slot) => {
        self.spills.push(StackVal(group.clone(), slot))
      },
      _ => ()
    }
  }
}
