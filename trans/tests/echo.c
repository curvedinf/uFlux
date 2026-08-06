/* echo: adapted from GNU coreutils src/echo.c to the trans.uf C subset.
   Supports -n, -e, -E (combinable), and the escapes \\ \a \b \c \f \n \r
   \t \v \0NNN \xHH.  Character comparisons use numeric codes to stay
   inside the subset's char-literal escape set. */

int isflag(char* s) {
  if (s[0] != 45) return 0;
  if (s[1] == 0) return 0;
  int j = 1;
  while (s[j] != 0) {
    int c = s[j];
    if (c != 110 && c != 101 && c != 69) return 0;
    j++;
  }
  return 1;
}

int hexdig(int c) {
  if (c >= 48 && c <= 57) return 1;
  if (c >= 97 && c <= 102) return 1;
  if (c >= 65 && c <= 70) return 1;
  return 0;
}

int hexv(int c) {
  if (c >= 48 && c <= 57) return c - 48;
  if (c >= 97 && c <= 102) return c - 87;
  if (c >= 65 && c <= 70) return c - 55;
  return 0;
}

int putesc(char* s) {
  int j = 0;
  while (s[j] != 0) {
    int c = s[j];
    if (c == 92 && s[j + 1] != 0) {
      int d = s[j + 1];
      j += 2;
      if (d == 110) putchar(10);
      else if (d == 116) putchar(9);
      else if (d == 114) putchar(13);
      else if (d == 92) putchar(92);
      else if (d == 97) putchar(7);
      else if (d == 98) putchar(8);
      else if (d == 102) putchar(12);
      else if (d == 118) putchar(11);
      else if (d == 99) return 1;
      else if (d == 48) {
        int v = 0;
        int k = 0;
        while (k < 3) {
          int o = s[j];
          if (o < 48 || o > 55) break;
          v = v * 8 + (o - 48);
          j++;
          k++;
        }
        putchar(v);
      }
      else if (d == 120) {
        int v = 0;
        int k = 0;
        while (k < 2) {
          int o = s[j];
          if (hexdig(o) == 0) break;
          v = v * 16 + hexv(o);
          j++;
          k++;
        }
        putchar(v);
      }
      else {
        putchar(92);
        putchar(d);
      }
    } else {
      putchar(c);
      j++;
    }
  }
  return 0;
}

int main() {
  int nonl = 0;
  int esc = 0;
  int i = 1;
  while (i < argc) {
    if (isflag(argv[i]) == 0) break;
    char* s = argv[i];
    int j = 1;
    while (s[j] != 0) {
      int c = s[j];
      if (c == 110) nonl = 1;
      if (c == 101) esc = 1;
      if (c == 69) esc = 0;
      j++;
    }
    i++;
  }
  int first = 1;
  while (i < argc) {
    if (first == 0) putchar(32);
    first = 0;
    if (esc) {
      if (putesc(argv[i])) return 0;
    } else {
      printf("%s", argv[i]);
    }
    i++;
  }
  if (nonl == 0) putchar(10);
  return 0;
}
