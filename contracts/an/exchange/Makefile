# Builds eccUSDCBridge, DepositVoucher, EthBeaconLightClient and
# ArbitrumBlockVerifier next to their sources.
#
# Each contract is built with the compiler its tracked artefact was built with,
# because any other one changes the code hash: eccUSDCBridge with sold 0.80.0
# (../0.80.0_compiled), DepositVoucher, EthBeaconLightClient and
# ArbitrumBlockVerifier with sold 0.81.0 (../0.81.0_compiled). The light client
# and the verifier call gosh.keccak256, so SOLD_0_81 has to be the 0.81.0 built
# from gosh-sh/TVM-Solidity-Compiler branch feature/keccak256-builtin. A build
# refuses a compiler that reports a different version. It does not touch the
# tracked artefacts: copy the rebuilt ones over by hand and commit them
# together.
#
#   make SOLD_0_80=/path/to/sold-0.80.0 SOLD_0_81=/path/to/sold-0.81.0

SOLD_0_80 ?=
SOLD_0_81 ?=
TVM_VERSION ?= gosh
TVCS := eccUSDCBridge DepositVoucher EthBeaconLightClient ArbitrumBlockVerifier

SOLD_eccUSDCBridge := $(SOLD_0_80)
SOLD_VERSION_eccUSDCBridge := 0.80.0
SOLD_DepositVoucher := $(SOLD_0_81)
SOLD_VERSION_DepositVoucher := 0.81.0
SOLD_EthBeaconLightClient := $(SOLD_0_81)
SOLD_VERSION_EthBeaconLightClient := 0.81.0
SOLD_ArbitrumBlockVerifier := $(SOLD_0_81)
SOLD_VERSION_ArbitrumBlockVerifier := 0.81.0

# The bridge imports the voucher, the light client and the verifier's deploy
# surface; the voucher, the light client and the verifier import it back.
SOURCES := $(wildcard *.sol) $(wildcard modifiers/*.sol) ../token/interface/ISubscriber.sol

.DEFAULT_GOAL := build
.PHONY: build clean

build: $(addsuffix .tvc,$(TVCS))

%.tvc %.abi.json: %.sol $(SOURCES)
	@test -n "$(SOLD_$*)" || { echo "$*: set $(if $(filter 0.80.0,$(SOLD_VERSION_$*)),SOLD_0_80,SOLD_0_81) to sold $(SOLD_VERSION_$*)"; exit 1; }
	@$(SOLD_$*) --version | grep -qF "sold $(SOLD_VERSION_$*)+" || { echo "$*: $(SOLD_$*) is not sold $(SOLD_VERSION_$*)"; exit 1; }
	$(SOLD_$*) --tvm-version $(TVM_VERSION) --base-path .. $< -o .
	rm -f $*.debug.json $*.code

clean:
	rm -f *.tvc *.abi.json *.code *.debug.json
