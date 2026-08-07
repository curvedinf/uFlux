import sys
from math import sqrt
def eval_A(i, j):
 return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)
def multiply_Av(v, n):
 return [sum(v[j] * eval_A(i, j) for j in range(n)) for i in range(n)]
def multiply_Atv(v, n):
 return [sum(v[j] * eval_A(j, i) for j in range(n)) for i in range(n)]
def multiply_AtAv(v, n):
 return multiply_Atv(multiply_Av(v, n), n)
def main():
 n = int(sys.argv[1]) if len(sys.argv) > 1 else 5500
 u = [1.0] * n
 v = [0.0] * n
 for _ in range(10):
  v = multiply_AtAv(u, n)
  u = multiply_AtAv(v, n)
 vBv = sum(u[i] * v[i] for i in range(n))
 vv = sum(v[i] * v[i] for i in range(n))
 sys.stdout.write("%.9f\n" % sqrt(vBv / vv))
main()
