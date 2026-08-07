import sys
n=int(sys.argv[1]) if len(sys.argv)>1 else 1000
count=0
for y in range(n):
 ci=2.0*y/n-1.0
 for x in range(n):
  cr=2.0*x/n-1.5
  zr=0.0;zi=0.0
  for _ in range(50):
   zr2=zr*zr;zi2=zi*zi
   if zr2+zi2>4.0:break
   zi=2.0*zr*zi+ci;zr=zr2-zi2+cr
  if zr*zr+zi*zi<=4.0:count+=1
print(count)
