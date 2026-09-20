# Builds eccUSDCBridge, DepositVoucher and EthBeaconLightClient next to their
# sources.
#
# All three are built with sold 0.82.0 (../0.82.0_compiled), the first compiler
# that carries both cross-dapp messages and the per-emit event version these
# sources now use; 0.80.0 and 0.81.0 no longer parse them. A build refuses a
# compiler that reports a different version, because any other one changes the
# code hash. It does not touch the tracked artefacts: move the rebuilt ones
# over by hand and commit them together.
#
#   make SOLD=/path/to/sold-0.82.0

SOLD ?=
SOLD_VERSION := 0.82.0
TVM_VERSION ?= gosh
TVCS := eccUSDCBridge DepositVoucher EthBeaconLightClient

# The bridge imports the voucher and the light client, and both import it back.
SOURCES := $(wildcard *.sol) $(wildcard modifiers/*.sol) ../token/interface/ISubscriber.sol

.DEFAULT_GOAL := build
.PHONY: build clean

build: $(addsuffix .tvc,$(TVCS))

%.tvc %.abi.json: %.sol $(SOURCES)
	@test -n "$(SOLD)" || { echo "$*: set SOLD to sold $(SOLD_VERSION)"; exit 1; }
	@$(SOLD) --version | grep -qF "sold $(SOLD_VERSION)+" || { echo "$*: $(SOLD) is not sold $(SOLD_VERSION)"; exit 1; }
	$(SOLD) --tvm-version $(TVM_VERSION) --base-path .. $< -o .
	rm -f $*.debug.json $*.code

clean:
	rm -f *.tvc *.abi.json *.code *.debug.json
