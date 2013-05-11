RUSTC ?= rustc
RUSTFLAGS ?= -O

TEST_BINARY ?= ./run-tests

SRC ?=
SRC += src/linearscan.rs
SRC += src/linearscan/graph.rs
SRC += src/linearscan/flatten.rs
SRC += src/linearscan/allocator.rs
SRC += src/tests.rs

all: $(TEST_BINARY)
	$(TEST_BINARY)

clean:
	rm -f $(TEST_BINARY)

$(TEST_BINARY): $(SRC)
	$(RUSTC) $(RUSTFLAGS) --test src/tests.rs -o $@


.PHONY: all clean
