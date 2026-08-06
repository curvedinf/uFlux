/* yes: adapted from GNU coreutils src/yes.c to the trans.uf C subset.
   Prints the arguments joined by spaces (or "y") forever. */

int main() {
  if (argc > 1) {
    while (1) {
      int i;
      for (i = 1; i < argc; i++) {
        if (i > 1) putchar(32);
        printf("%s", argv[i]);
      }
      putchar(10);
    }
  }
  while (1) {
    puts("y");
  }
  return 0;
}
