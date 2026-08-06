import sys
from collections import defaultdict
path=sys.argv[1] if len(sys.argv)>1 else "bench/data/sales.csv"
tr=tu=0
trev=0.0
rr=defaultdict(float)
pr=defaultdict(float)
cr=defaultdict(float)
with open(path,"r",encoding="utf-8",errors="replace",buffering=1048576) as f:
 next(f)
 for line in f:
  c=line.split(",")
  tr+=1
  t=float(c[10])
  trev+=t
  tu+=int(c[7])
  rr[c[2]]+=t
  pr[c[4]]+=t
  cr[c[3]]+=t
ao=trev/tr if tr else 0.0
o=["=== DATA ANALYTICS RESULTS ===",f"total_rows: {tr}",f"total_revenue: {trev:.2f}",f"avg_order: {ao:.2f}",f"total_units: {tu}"]
for i,(n,v) in enumerate(sorted(rr.items(),key=lambda x:(-x[1],x[0])),1):o.append(f"region_{i}: {n} ${v:.2f}")
for i,(n,v) in enumerate(sorted(pr.items(),key=lambda x:(-x[1],x[0]))[:5],1):o.append(f"product_{i}: {n} ${v:.2f}")
for i,(n,v) in enumerate(sorted(cr.items(),key=lambda x:(-x[1],x[0]))[:3],1):o.append(f"country_{i}: {n} ${v:.2f}")
sys.stdout.write("\n".join(o)+"\n")
