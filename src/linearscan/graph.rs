use extra::smallintmap::SmallIntMap;
use extra::bitv::BitvSet;

pub type BlockId = uint;
pub type InstrId = uint;
pub type IntervalId = uint;
pub type GroupId = uint;
pub type RegisterId = uint;
pub type StackId = uint;

pub struct Graph<K> {
  root: Option<BlockId>,
  block_id: BlockId,
  instr_id: InstrId,
  interval_id: IntervalId,
  intervals: ~SmallIntMap<~Interval>,
  blocks: ~SmallIntMap<~Block<K> >,
  instructions: ~SmallIntMap<~Instruction<K> >,
  gaps: ~SmallIntMap<~GapState>,
  physical: ~SmallIntMap<~SmallIntMap<IntervalId> >
}

pub struct BlockBuilder<'self, K> {
  graph: &'self mut Graph<K>,
  block: BlockId
}

pub struct Block<K> {
  id: BlockId,
  instructions: ~[InstrId],
  successors: ~[BlockId],
  predecessors: ~[BlockId],

  // Fields for flattener
  loop_index: uint,
  loop_depth: uint,
  incoming_forward_branches: uint,

  // Fields for liveness analysis
  live_gen: ~BitvSet,
  live_kill: ~BitvSet,
  live_in: ~BitvSet,
  live_out: ~BitvSet,

  ended: bool
}

pub struct Instruction<K> {
  id: InstrId,
  block: BlockId,
  kind: InstrKind<K>,
  output: Option<IntervalId>,
  inputs: ~[IntervalId],
  temporary: ~[IntervalId],
  added: bool
}

// Abstraction to allow having user-specified instruction types
// as well as internal movement instructions
#[deriving(ToStr)]
pub enum InstrKind<K> {
  User(K),
  Gap,
  Phi(GroupId),
  ToPhi(GroupId)
}

pub struct Interval {
  id: IntervalId,
  value: Value,
  ranges: ~[LiveRange],
  parent: Option<IntervalId>,
  uses: ~[Use],
  children: ~[IntervalId],
  fixed: bool
}

#[deriving(Eq)]
pub enum Value {
  Virtual(GroupId),
  Register(GroupId, RegisterId),
  Stack(GroupId, StackId)
}

pub struct Use {
  kind: UseKind,
  pos: InstrId
}

#[deriving(Eq)]
pub enum UseKind {
  UseAny(GroupId),
  UseRegister(GroupId),
  UseFixed(GroupId, RegisterId)
}

pub struct LiveRange {
  start: InstrId,
  end: InstrId
}

pub struct GapState {
  actions: ~[GapAction]
}

#[deriving(Eq)]
pub enum GapActionKind {
  Move,
  Swap
}

pub struct GapAction {
  kind: GapActionKind,
  from: IntervalId,
  to: IntervalId
}

pub trait KindHelper {
  fn clobbers(&self, group: GroupId) -> bool;
  fn temporary(&self) -> ~[GroupId];
  fn use_kind(&self, i: uint) -> UseKind;
  fn result_kind(&self) -> Option<UseKind>;
}

pub impl<K: KindHelper+Copy+ToStr> Graph<K> {
  /// Create new graph
  fn new() -> Graph<K> {
    Graph {
      root: None,
      block_id: 0,
      instr_id: 0,
      interval_id: 0,
      intervals: ~SmallIntMap::new(),
      blocks: ~SmallIntMap::new(),
      instructions: ~SmallIntMap::new(),
      gaps: ~SmallIntMap::new(),
      physical: ~SmallIntMap::new()
    }
  }

  /// Create empty block
  fn empty_block(&mut self) -> BlockId {
    let block = ~Block::new(self);
    let id = block.id;
    self.blocks.insert(id, block);
    return id;
  }

  /// Create empty block and initialize it in the block
  fn block(&mut self, body: &fn(b: &mut BlockBuilder<K>)) -> BlockId {
    let block = ~Block::new(self);
    let id = block.id;
    self.blocks.insert(id, block);

    // Execute body
    self.with_block(id, body);

    return id;
  }

  /// Create phi value
  fn phi(&mut self, group: GroupId) -> InstrId {
    let res = Instruction::new(self, Phi(group), ~[]);
    // Prevent adding phi to block
    self.get_instr(&res).added = true;
    return res;
  }

  /// Perform operations on block
  fn with_block(&mut self, id: BlockId, body: &fn(b: &mut BlockBuilder<K>)) {
    let mut b = BlockBuilder {
      graph: self,
      block: id
    };
    body(&mut b);
  }

  /// Create new instruction outside the block
  fn new_instr(&mut self, kind: K, args: ~[InstrId]) -> InstrId {
    return Instruction::new(self, User(kind), args);
  }

  /// Set graph's root block
  fn set_root(&mut self, id: BlockId) {
    self.root = Some(id);
  }

  /// Create gap (internal)
  fn create_gap(&mut self, block: &BlockId) -> ~Instruction<K> {
    let id = self.instr_id();
    self.gaps.insert(id, ~GapState { actions: ~[] });
    return ~Instruction {
      id: id,
      block: *block,
      kind: Gap,
      output: None,
      inputs: ~[],
      temporary: ~[],
      added: true
    };
  }

  /// Mutable block getter
  fn get_block<'r>(&'r mut self, id: &BlockId) -> &'r mut ~Block<K> {
    self.blocks.find_mut(id).unwrap()
  }

  /// Mutable instruction getter
  fn get_instr<'r>(&'r mut self, id: &InstrId) -> &'r mut ~Instruction<K> {
    self.instructions.find_mut(id).unwrap()
  }

  /// Mutable interval getter
  fn get_interval<'r>(&'r mut self, id: &IntervalId) -> &'r mut ~Interval {
    self.intervals.find_mut(id).unwrap()
  }

  /// Mutable gap state getter
  fn get_gap<'r>(&'r mut self, id: &InstrId) -> &'r mut ~GapState {
    self.gaps.find_mut(id).unwrap()
  }

  /// Find next intersection of two intervals
  fn get_intersection(&self,
                      a: &IntervalId,
                      b: &IntervalId) -> Option<InstrId> {
    let int_a = self.intervals.get(a);
    let int_b = self.intervals.get(b);

    for int_a.ranges.each() |a| {
      for int_b.ranges.each() |b| {
        match a.get_intersection(b) {
          Some(pos) => {
            return Some(pos)
          },
          _ => ()
        }
      }
    }

    return None;
  }

  /// Return `true` if `pos` is either some block's start or end
  fn block_boundary(&self, pos: InstrId) -> bool {
    let block = self.blocks.get(&self.instructions.get(&pos).block);
    return block.start() == pos || block.end() == pos;
  }

  /// Find optimal split position between two instructions
  fn optimal_split_pos(&self,
                       group: GroupId,
                       start: InstrId,
                       end: InstrId) -> InstrId {
    // Fast and unfortunate case
    if start == end {
      return end;
    }

    let mut best_pos = end;
    let mut best_depth = uint::max_value;
    for self.blocks.each() |_, block| {
      if best_depth >= block.loop_depth {
        let block_to = block.end();

        // Choose the most shallow block
        if start < block_to && block_to <= end {
          best_pos = block_to;
          best_depth = block.loop_depth;
        }
      }
    }

    // Always split at gap
    if !self.is_gap(&best_pos) && !self.clobbers(group, &best_pos) {
      assert!(best_pos >= start + 1);
      best_pos -= 1;
    }
    assert!(start < best_pos && best_pos <= end);
    return best_pos;
  }

  /// Split interval or one of it's children at specified position, return
  /// id of split child.
  fn split_at(&mut self, id: &IntervalId, pos: InstrId) -> IntervalId {
    // We should always make progress
    assert!(self.intervals.get(id).start() < pos);

    // Split could be either at gap or at call
    let group = self.intervals.get(id).value.group();
    assert!(self.is_gap(&pos) || self.clobbers(group, &pos));

    let child = Interval::new(self, group);
    let parent = match self.get_interval(id).parent {
      Some(parent) => parent,
      None => *id
    };

    // Find appropriate child interval
    let mut split_parent = parent;
    if !self.intervals.get(&split_parent).covers(pos) {
      for self.intervals.get(&split_parent).children.each() |child| {
        if self.intervals.get(child).covers(pos) {
          split_parent = *child;
        }
      }
      assert!(self.intervals.get(&split_parent).covers(pos));
    }

    // Insert movement
    let split_at_call = self.clobbers(group, &pos);
    if split_at_call || !self.block_boundary(pos) {
      self.get_gap(&pos).add_move(&split_parent, &child);
    }

    // Move out ranges
    let mut child_ranges =  ~[];
    let parent_ranges =
        do self.intervals.get(&split_parent).ranges.filter_mapped |range| {
      if range.end <= pos {
        Some(*range)
      } else if range.start < pos {
        // Split required
        child_ranges.push(LiveRange {
          start: pos,
          end: range.end
        });
        Some(LiveRange {
          start: range.start,
          end: pos
        })
      } else {
        child_ranges.push(*range);
        None
      }
    };

    // Ensure that at least one range is always present
    assert!(child_ranges.len() != 0);
    assert!(parent_ranges.len() != 0);
    self.get_interval(&child).ranges = child_ranges;
    self.get_interval(&split_parent).ranges = parent_ranges;

    // Move out uses
    let mut child_uses =  ~[];
    let split_on_call = self.instructions.get(&pos).kind.clobbers(group);
    let parent_uses =
        do self.intervals.get(&split_parent).uses.filter_mapped |u| {
      if split_on_call && u.pos <= pos || !split_on_call && u.pos < pos {
        Some(*u)
      } else {
        child_uses.push(*u);
        None
      }
    };
    self.get_interval(&child).uses = child_uses;
    self.get_interval(&split_parent).uses = parent_uses;

    // Add child
    let mut index = 0;
    for self.intervals.get(&parent).children.eachi_reverse() |i, child| {
      if self.intervals.get(child).end() <= pos {
        index = i + 1;
        break;
      }
    };
    self.get_interval(&parent).children.insert(index, child);
    self.get_interval(&child).parent = Some(parent);

    return child;
  }

  /// Helper function
  fn iterate_children(&self,
                      id: &IntervalId,
                      f: &fn(&~Interval) -> bool) -> bool {
    let p = self.intervals.get(id);
    if !f(p) {
      return false;
    }

    for p.children.each() |child_id| {
      let child = self.intervals.get(child_id);
      if !f(child) { break; }
    }

    true
  }

  /// Find child interval, that covers specified position
  fn child_at(&self, parent: &IntervalId, pos: InstrId) -> Option<IntervalId> {
    for self.iterate_children(parent) |interval| {
      if interval.start() <= pos && pos < interval.end() {
        return Some(interval.id);
      }
    };

    // No match?
    None
  }

  fn child_with_use_at(&self,
                       parent: &IntervalId,
                       pos: InstrId) -> Option<IntervalId> {
    for self.iterate_children(parent) |interval| {
      if interval.start() <= pos && pos <= interval.end() &&
         interval.uses.any(|u| { u.pos == pos }) {
        return Some(interval.id);
      }
    };

    // No match?
    None
  }

  fn get_value(&self, i: &IntervalId, pos: InstrId) -> Option<Value> {
    let child = self.child_with_use_at(i, pos);
    match child {
      Some(child) => Some(self.intervals.get(&child).value),
      None => None
    }
  }

  /// Return true if instruction at specified position is Gap
  fn is_gap(&self, pos: &InstrId) -> bool {
    match self.instructions.get(pos).kind {
      Gap => true,
      _ => false
    }
  }

  /// Return true if instruction at specified position contains
  /// register-clobbering call.
  fn clobbers(&self, group: GroupId, pos: &InstrId) -> bool {
    return self.instructions.get(pos).kind.clobbers(group);
  }

  /// Return next block id, used at graph construction
  #[inline(always)]
  priv fn block_id(&mut self) -> BlockId {
    let r = self.block_id;
    self.block_id += 1;
    return r;
  }

  /// Return next instruction id, used at graph construction
  #[inline(always)]
  priv fn instr_id(&mut self) -> InstrId {
    let r = self.instr_id;
    self.instr_id += 1;
    return r;
  }

  /// Return next interval id, used at graph construction
  #[inline(always)]
  priv fn interval_id(&mut self) -> IntervalId {
    let r = self.interval_id;
    self.interval_id += 1;
    return r;
  }
}

pub impl<'self, K: KindHelper+Copy+ToStr> BlockBuilder<'self, K> {
  /// add instruction to block
  fn add(&mut self, kind: K, args: ~[InstrId]) -> InstrId {
    let instr_id = self.graph.new_instr(kind, args);

    self.add_existing(instr_id);

    return instr_id;
  }

  /// add existing instruction to block
  fn add_existing(&mut self, instr_id: InstrId) {
    assert!(!self.graph.get_instr(&instr_id).added);
    self.graph.get_instr(&instr_id).added = true;
    self.graph.get_instr(&instr_id).block = self.block;

    let block = self.graph.get_block(&self.block);
    assert!(!block.ended);
    block.instructions.push(instr_id);
  }

  /// add arg to existing instruction in block
  fn add_arg(&mut self, id: InstrId, arg: InstrId) {
    assert!(self.graph.instructions.get(&id).block == self.block);
    self.graph.get_instr(&id).inputs.push(arg);
  }

  /// add phi movement to block
  fn to_phi(&mut self, input: InstrId, phi: InstrId) {
    let group = match self.graph.get_instr(&phi).kind {
      Phi(group) => group,
      _ => fail!("Expected Phi argument")
    };
    let out = self.graph.instructions.get(&phi).output.expect("Phi output");

    let res = Instruction::new_empty(self.graph, ToPhi(group), ~[input]);
    self.graph.get_instr(&res).output = Some(out);
    self.add_existing(res);
  }

  /// end block
  fn end(&mut self) {
    let block = self.graph.get_block(&self.block);
    assert!(!block.ended);
    assert!(block.instructions.len() > 0);
    block.ended = true;
  }

  /// add `target_id` to block's successors
  fn goto(&mut self, target_id: BlockId) {
    self.graph.get_block(&self.block).add_successor(target_id);
    self.graph.get_block(&target_id).add_predecessor(self.block);
    self.end();
  }

  /// add `left` and `right` to block's successors
  fn branch(&mut self, left: BlockId, right: BlockId) {
    self.graph.get_block(&self.block).add_successor(left)
                                     .add_successor(right);
    self.graph.get_block(&left).add_predecessor(self.block);
    self.graph.get_block(&right).add_predecessor(self.block);
    self.end();
  }

  /// mark block as root
  fn make_root(&mut self) {
    self.graph.set_root(self.block);
  }
}

pub impl<K: KindHelper+Copy+ToStr> Block<K> {
  /// Create new empty block
  fn new(graph: &mut Graph<K>) -> Block<K> {
    Block {
      id: graph.block_id(),
      instructions: ~[],
      successors: ~[],
      predecessors: ~[],
      loop_index: 0,
      loop_depth: 0,
      incoming_forward_branches: 0,
      live_gen: ~BitvSet::new(),
      live_kill: ~BitvSet::new(),
      live_in: ~BitvSet::new(),
      live_out: ~BitvSet::new(),
      ended: false
    }
  }

  fn add_successor<'r>(&'r mut self, succ: BlockId) -> &'r mut Block<K> {
    assert!(self.successors.len() <= 2);
    self.successors.push(succ);
    return self;
  }

  fn add_predecessor(&mut self, pred: BlockId) {
    assert!(self.predecessors.len() <= 2);
    self.predecessors.push(pred);
    // NOTE: we'll decrease them later in flatten.rs
    self.incoming_forward_branches += 1;
  }
}

pub impl<K: KindHelper+Copy+ToStr> Instruction<K> {
  /// Create instruction without output interval
  fn new_empty(graph: &mut Graph<K>,
               kind: InstrKind<K>,
               args: ~[InstrId]) -> InstrId {
    let id = graph.instr_id();

    let inputs = do vec::map(args) |input_id| {
      let output = graph.instructions.get(input_id).output
                        .expect("Instruction should have output");
      graph.get_interval(&output);
      output
    };

    let mut temporary = ~[];
    for kind.temporary().each() |group| {
      temporary.push(Interval::new(graph, *group));
    }

    let r = Instruction {
      id: id,
      block: 0,
      kind: kind,
      output: None,
      inputs: inputs,
      temporary: temporary,
      added: false
    };
    graph.instructions.insert(r.id, ~r);
    return id;
  }

  /// Create instruction with output
  fn new(graph: &mut Graph<K>,
         kind: InstrKind<K>,
         args: ~[InstrId]) -> InstrId {

    let output = match kind.result_kind() {
      Some(k) => Some(Interval::new(graph, k.group())),
      None => None
    };

    let instr = Instruction::new_empty(graph, kind, args);
    graph.get_instr(&instr).output = output;
    return instr;
  }
}

pub impl Interval {
  /// Create new virtual interval
  fn new<K: KindHelper+Copy+ToStr>(graph: &mut Graph<K>,
                                   group: GroupId) -> IntervalId {
    let r = Interval {
      id: graph.interval_id(),
      value: Virtual(group),
      ranges: ~[],
      parent: None,
      uses: ~[],
      children: ~[],
      fixed: false
    };
    let id = r.id;
    graph.intervals.insert(r.id, ~r);
    return id;
  }

  /// Add range to interval's live range list.
  /// NOTE: Ranges are ordered by start position
  fn add_range(&mut self, start: InstrId, end: InstrId) {
    assert!(self.ranges.len() == 0 || self.ranges.head().start >= end);

    // Extend last range
    if self.ranges.len() > 0 && self.ranges.head().start == end {
      self.ranges[0].start = start;
    } else {
      // Insert new range
      self.ranges.unshift(LiveRange { start: start, end: end });
    }
  }

  /// Return mutable first range
  fn first_range<'r>(&'r mut self) -> &'r mut LiveRange {
    assert!(self.ranges.len() != 0);
    return &mut self.ranges[0];
  }

  /// Return interval's start position
  fn start(&self) -> InstrId {
    assert!(self.ranges.len() != 0);
    return self.ranges.head().start;
  }

  /// Return interval's end position
  fn end(&self) -> InstrId {
    assert!(self.ranges.len() != 0);
    return self.ranges.last().end;
  }

  /// Return true if one of the ranges contains `pos`
  fn covers(&self, pos: InstrId) -> bool {
    return do self.ranges.any() |range| {
      range.covers(pos)
    };
  }

  /// Add use to the interval's use list.
  /// NOTE: uses are ordered by increasing `pos`
  fn add_use(&mut self, kind: UseKind, pos: InstrId) {
    assert!(self.uses.len() == 0 ||
            self.uses[0].pos > pos ||
            self.uses[0].kind.group() == kind.group());
    self.uses.unshift(Use { kind: kind, pos: pos });
  }

  /// Return next UseFixed(...) after `after` position.
  fn next_fixed_use(&self, after: InstrId) -> Option<Use> {
    for self.uses.each() |u| {
      match u.kind {
        UseFixed(_, _) if u.pos >= after => { return Some(*u); },
        _ => ()
      }
    };
    return None;
  }

  /// Return next UseFixed(...) or UseRegister after `after` position.
  fn next_use(&self, after: InstrId) -> Option<Use> {
    for self.uses.each() |u| {
      if u.pos >= after && !u.kind.is_any() {
        return Some(*u);
      }
    };
    return None;
  }

  /// Return last UseFixed(...) or UseRegister before `before` position
  fn last_use(&self, before: InstrId) -> Option<Use> {
    for self.uses.each_reverse() |u| {
      if u.pos <= before && !u.kind.is_any() {
        return Some(*u);
      }
    };
    return None;
  }
}

impl<K: KindHelper+Copy+ToStr> KindHelper for InstrKind<K> {
  /// Return true if instruction is clobbering registers
  fn clobbers(&self, group: GroupId) -> bool {
    match self {
      &User(ref k) => k.clobbers(group),
      &Gap => false,
      &ToPhi(_) => false,
      &Phi(_) => false
    }
  }

  /// Return count of instruction's temporary operands
  fn temporary(&self) -> ~[GroupId] {
    match self {
      &User(ref k) => k.temporary(),
      &Gap => ~[],
      &Phi(_) => ~[],
      &ToPhi(_) => ~[]
    }
  }

  /// Return use kind of instruction's `i`th input
  fn use_kind(&self, i: uint) -> UseKind {
    match self {
      &User(ref k) => k.use_kind(i),
      &Gap => UseAny(0), // note: group is not important for gap
      &Phi(g) => UseAny(g),
      &ToPhi(g) => UseAny(g)
    }
  }

  /// Return result kind of instruction or None, if instruction has no result
  fn result_kind(&self) -> Option<UseKind> {
    match self {
      &User(ref k) => k.result_kind(),
      &Gap => None,
      &Phi(g) => Some(UseAny(g)),
      &ToPhi(g) => Some(UseAny(g))
    }
  }
}

impl LiveRange {
  /// Return true if range contains position
  fn covers(&self, pos: InstrId) -> bool {
    return self.start <= pos && pos < self.end;
  }

  /// Return first intersection position of two ranges
  fn get_intersection(&self, other: &LiveRange) -> Option<InstrId> {
    if self.covers(other.start) {
      return Some(other.start);
    } else if other.start < self.start && self.start < other.end {
      return Some(self.start);
    }
    return None;
  }
}

impl Value {
  fn is_virtual(&self) -> bool {
    match self {
      &Virtual(_) => true,
      _ => false
    }
  }

  fn group(&self) -> GroupId {
    match self {
      &Virtual(g) => g,
      &Register(g, _) => g,
      &Stack(g, _) => g
    }
  }
}

impl UseKind {
  fn is_fixed(&self) -> bool {
    match self {
      &UseFixed(_, _) => true,
      _ => false
    }
  }

  fn is_any(&self) -> bool {
    match self {
      &UseAny(_) => true,
      _ => false
    }
  }

  fn group(&self) -> GroupId {
    match self {
      &UseRegister(g) => g,
      &UseAny(g) => g,
      &UseFixed(g, _) => g
    }
  }
}

pub impl GapState {
  fn add_move(&mut self, from: &InstrId, to: &InstrId) {
    self.actions.push(GapAction { kind: Move, from: *from, to: *to });
  }
}

pub impl<K: KindHelper+Copy+ToStr> Block<K> {
  fn start(&self) -> InstrId {
    assert!(self.instructions.len() != 0);
    return *self.instructions.head();
  }

  fn end(&self) -> InstrId {
    assert!(self.instructions.len() != 0);
    return *self.instructions.last() + 1;
  }
}

impl Eq for LiveRange {
  #[inline(always)]
  fn eq(&self, other: &LiveRange) -> bool {
    self.start == other.start && self.end == other.end
  }

  #[inline(always)]
  fn ne(&self, other: &LiveRange) -> bool { !self.eq(other) }
}

// LiveRange is ordered by start position
impl Ord for LiveRange {
  #[inline(always)]
  fn lt(&self, other: &LiveRange) -> bool { self.start < other.start }

  #[inline(always)]
  fn gt(&self, other: &LiveRange) -> bool { self.start > other.start }

  #[inline(always)]
  fn le(&self, other: &LiveRange) -> bool { !self.gt(other) }

  #[inline(always)]
  fn ge(&self, other: &LiveRange) -> bool { !self.lt(other) }
}
