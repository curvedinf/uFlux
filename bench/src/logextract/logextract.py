import sys
from collections import Counter
path=sys.argv[1] if len(sys.argv)>1 else "bench/data/access.log"
tl=tb=0
sc={2:0,3:0,4:0,5:0}
hc=[0]*24
ic=Counter()
with open(path,"r",encoding="utf-8",errors="replace") as f:
 for line in f:
  t=line.split(" ")
  if len(t)<10:continue
  ip=t[0]
  hour=int(t[3].split(":")[1])
  status=int(t[8])
  nb=int(t[9])
  tl+=1
  tb+=nb
  sc[status//100]+=1
  hc[hour]+=1
  ic[ip]+=1
o=["=== LOG EXTRACTION RESULTS ===",f"total_lines: {tl}",f"total_bytes: {tb}",f"status_2xx: {sc[2]}",f"status_3xx: {sc[3]}",f"status_4xx: {sc[4]}",f"status_5xx: {sc[5]}"]
for h in range(24):o.append(f"hour_{h:02d}: {hc[h]}")
for i,(ip,c) in enumerate(sorted(ic.items(),key=lambda x:(-x[1],x[0]))[:10],1):o.append(f"top_ip_{i}: {ip} ({c})")
sys.stdout.write("\n".join(o)+"\n")
