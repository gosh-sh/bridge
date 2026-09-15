# Builds eccUSDCBridge and DepositVoucher next to their sources.
#
# The artefacts that go into a zerostate are the ones under ../0.80.0_compiled
# and ../0.81.0_compiled. A build here does not touch them: copy a rebuilt
# pair over by hand, together, and commit both.
#
#   make SOLD=/path/to/sold

SOLD ?= sold
TVM_VERSION ?= gosh
TVCS := eccUSDCBridge DepositVoucher

# Each contract imports the other, the modifiers and the subscriber interface.
SOURCES := $(addsuffix .sol,$(TVCS)) $(wildcard modifiers/*.sol) ../token/interface/ISubscriber.sol

.DEFAULT_GOAL := build
.PHONY: build clean

build: $(addsuffix .tvc,$(TVCS))

%.tvc %.abi.json: %.sol $(SOURCES)
	$(SOLD) --tvm-version $(TVM_VERSION) --base-path .. $< -o .
	rm -f $*.debug.json $*.code

clean:
	rm -f *.tvc *.abi.json *.code *.debug.json
