# Copyright (c) Facebook, Inc. and its affiliates.
#
# This source code is licensed under the MIT license found in the
# LICENSE file in the root directory of this source tree.

TESTS_DIR=../../..

SOURCES = $(sort $(wildcard *.c) $(wildcard */*.c))
CLEAN_EXTRA = metadata.test

.PHONY: capture
capture:
	$(QUIET)$(call silent_on_success,Capturing with --store-textual in $(TEST_REL_DIR),\
	  $(REMOVE_DIR) infer-out $(EXPORT_DIR) && \
	  $(foreach src,$(SOURCES),\
	    $(INFER_BIN) capture --store-textual --reactive \
	      --project-root $(TESTS_DIR) -- clang -c $(src) -o $(src:.c=.o) &&) \
	  true)

metadata.test: export
	$(QUIET)rg -o "__sil_metadata_[a-z_]+" $(EXPORT_DIR)/metadata.sil | sort -u > metadata.test
	$(QUIET)diff -u metadata.exp metadata.test || \
	  (printf '$(TERM_ERROR)metadata mismatch$(TERM_RESET)\n' >&2; exit 1)

test: metadata.test

include $(TESTS_DIR)/export-textual.make
