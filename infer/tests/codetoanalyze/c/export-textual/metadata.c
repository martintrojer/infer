/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

int metadata_scope(int *p, int n) {
  int x = 0;
  while (n > 0) {
    int y = *p;
    if (y == 7) {
      x = y;
    }
    n--;
  }
  return x;
}
