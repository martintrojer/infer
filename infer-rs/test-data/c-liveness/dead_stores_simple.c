// Simple dead store test cases for liveness analysis.
// Used by the test harness: `infer capture --dump-textual` produces .sil from this.

void easy_bad(void) {
    int x = 5;
}

int dead_then_live_bad(void) {
    int x = 5;
    x = 3;
    return x;
}

int use_then_dead_bad(void) {
    int x = 5;
    int y = x;
    x = 7;
    return y;
}

void nested_dead_bad(void) {
    int x = 5;
    if (x > 3) {
        int y = 10;
    }
}

void param_reassign_bad(int x) {
    x = 5;
}
