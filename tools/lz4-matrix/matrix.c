/* For every <n>.raw / <n>.lz4 pair in a directory, recompress <n>.raw with
 * LZ4_compress_HC of the liblz4 release this file is compiled against, at
 * every level, and count which levels reproduce <n>.lz4 byte for byte.
 * usage: matrix <release-tag> <dir> */
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lz4.h"
#include "lz4hc.h"

#if defined(LZ4HC_CLEVEL_MAX)
#define MAX_LEVEL LZ4HC_CLEVEL_MAX
#elif defined(LZ4HC_MAX_CLEVEL)
#define MAX_LEVEL LZ4HC_MAX_CLEVEL
#else
#define MAX_LEVEL 16
#endif

static char* slurp(const char* path, int* len) {
  FILE* f = fopen(path, "rb");
  if (!f) return NULL;
  fseek(f, 0, SEEK_END);
  *len = (int)ftell(f);
  rewind(f);
  char* buf = malloc(*len + 1);
  if (fread(buf, 1, *len, f) != (size_t)*len) *len = -1;
  fclose(f);
  return buf;
}

int main(int argc, char** argv) {
  if (argc != 3) {
    fprintf(stderr, "usage: %s <release-tag> <dir>\n", argv[0]);
    return 2;
  }
  int hits[MAX_LEVEL + 1] = {0}, blocks = 0, any = 0;
  DIR* dir = opendir(argv[2]);
  if (!dir) return 1;
  struct dirent* entry;
  char path[4096];
  while ((entry = readdir(dir))) {
    size_t name_len = strlen(entry->d_name);
    if (name_len < 5 || strcmp(entry->d_name + name_len - 4, ".raw")) continue;
    int raw_len, lz4_len;
    snprintf(path, sizeof path, "%s/%s", argv[2], entry->d_name);
    char* raw = slurp(path, &raw_len);
    strcpy(path + strlen(path) - 4, ".lz4");
    char* lz4 = slurp(path, &lz4_len);
    if (!raw || !lz4 || raw_len < 0 || lz4_len < 0) return 1;
    int bound = LZ4_compressBound(raw_len), matched = 0;
    char* out = malloc(bound);
    for (int level = 1; level <= MAX_LEVEL; level++) {
      int n = LZ4_compress_HC(raw, out, raw_len, bound, level);
      if (n == lz4_len && !memcmp(out, lz4, n)) hits[level]++, matched = 1;
    }
    blocks++;
    any += matched;
    free(out);
    free(raw);
    free(lz4);
  }
  closedir(dir);
  printf("%-9s %d/%d blocks reproduced;", argv[1], any, blocks);
  for (int level = 1; level <= MAX_LEVEL; level++)
    if (hits[level]) printf(" L%d=%d", level, hits[level]);
  printf("\n");
  return 0;
}
