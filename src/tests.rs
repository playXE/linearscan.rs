extern mod std;

use linearscan::{Allocator, Config, Graph, KindHelper,
                 UseKind, UseAny, UseRegister};
use std::json::ToJson;
mod linearscan;

#[deriving(Eq, ToStr)]
enum Kind {
  Phi,
  Increment,
  BranchIfBigger,
  PrintHello,
  Zero,
  Ten,
  Goto,
  Return
}

impl KindHelper for Kind {
  fn is_call(&self) -> bool {
    match self {
      &PrintHello => true,
      _ => false
    }
  }

  fn tmp_count(&self) -> uint {
    match self {
      &BranchIfBigger => 1,
      _ => 0
    }
  }

  fn use_kind(&self, _: uint) -> UseKind {
    UseAny
  }

  fn result_kind(&self) -> Option<UseKind> {
    match self {
      &Goto => None,
      &Return => None,
      &BranchIfBigger => None,
      &PrintHello => None,
      _ => Some(UseRegister)
    }
  }
}

fn graph_test(body: &fn(b: &mut Graph<Kind>)) {
  let mut g = ~Graph::new::<Kind>();

  body(&mut *g);

  g.allocate(Config { register_count: 4 });
  io::println(g.to_json().to_str());
}

#[test]
fn realword_example() {
  do graph_test() |g| {
    let phi = g.phi();

    let cond = g.empty_block();
    let left = g.empty_block();
    let right = g.empty_block();

    do g.block() |b| {
      b.make_root();

      let zero = b.add(Zero, ~[]);
      b.to_phi(zero, phi);
      b.add(Goto, ~[]);
      b.goto(cond);
    };

    do g.with_block(cond) |b| {
      let ten = b.add(Ten, ~[]);
      b.add(BranchIfBigger, ~[phi, ten]);
      b.branch(left, right);
    };

    do g.with_block(left) |b| {
      let counter = b.add(Increment, ~[phi]);
      b.to_phi(counter, phi);
      b.add(Goto, ~[]);
      b.goto(cond);
    };

    do g.with_block(right) |b| {
      b.add(PrintHello, ~[]);
      b.add(Return, ~[]);
      b.end();
    };
  };
}
