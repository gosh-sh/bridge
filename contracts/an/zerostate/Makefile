# Builds the zerostate data encoder. It is never deployed — acki-nacki's
# zerostate generator runs it in tvm-debugger to build the cell the premined
# bridge is upgraded with — so its code hash does not matter and any sold that
# compiles it will do.
#
# tvm-debugger rewrites the data cell of the image it is given, so the
# tracked artefacts must be built by `make` and never used as the debugger's
# input in place; run the debugger on a copy.
#
#   make -C contracts/an/zerostate SOLD=/path/to/sold

SOLD ?= sold
TVM_VERSION ?= gosh

.DEFAULT_GOAL := build
.PHONY: build clean

build: BridgeZerostateData.tvc

BridgeZerostateData.tvc BridgeZerostateData.abi.json: BridgeZerostateData.sol
	$(SOLD) --tvm-version $(TVM_VERSION) $< -o .
	rm -f BridgeZerostateData.debug.json BridgeZerostateData.code

clean:
	rm -f *.code *.debug.json
