#!/usr/bin/env python3
import os,sys,json,subprocess,time,platform
from pathlib import Path
PROJ=Path("/home/chase/Projects/uflux")
BENCH=PROJ/"bench"
DATA=BENCH/"data"
RESULTS=BENCH/"results"
UF=PROJ/"comp"/"target"/"debug"/"uf"
RESULTS.mkdir(exist_ok=True)
from transformers import AutoTokenizer
tok=AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")
def count_tokens(path):
    with open(path,"r",encoding="utf-8",errors="replace") as f:
        text=f.read()
    return len(tok.encode(text)),len(text)
def count_lines(path):
    with open(path,"rb") as f:
        return sum(1 for _ in f)
def run_cmd(cmd,cwd=PROJ,timeout=120):
    t0=time.perf_counter()
    try:
        r=subprocess.run(cmd,cwd=cwd,capture_output=True,text=True,timeout=timeout)
        elapsed=time.perf_counter()-t0
        return r.returncode,elapsed,r.stdout.strip(),r.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1,timeout,"","TIMEOUT"
    except Exception as e:
        return -2,time.perf_counter()-t0,"",str(e)
def compile_all():
    print("=== Compiling ===")
    results={}
    le_cpp_src=BENCH/"src"/"logextract"/"logextract.cpp"
    le_cpp_bin=BENCH/"src"/"logextract"/"logextract_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(le_cpp_bin),str(le_cpp_src)])
    results["cpp_logextract"]=(rc==0,t)
    print(f"  C++ logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_cpp_src=BENCH/"src"/"analytics"/"analytics.cpp"
    an_cpp_bin=BENCH/"src"/"analytics"/"analytics_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(an_cpp_bin),str(an_cpp_src)])
    results["cpp_analytics"]=(rc==0,t)
    print(f"  C++ analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    le_rs_src=BENCH/"src"/"logextract"/"logextract.rs"
    le_rs_bin=BENCH/"src"/"logextract"/"logextract_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(le_rs_bin),str(le_rs_src)])
    results["rs_logextract"]=(rc==0,t)
    print(f"  Rust logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_rs_src=BENCH/"src"/"analytics"/"analytics.rs"
    an_rs_bin=BENCH/"src"/"analytics"/"analytics_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(an_rs_bin),str(an_rs_src)])
    results["rs_analytics"]=(rc==0,t)
    print(f"  Rust analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    le_uf_src=BENCH/"src"/"logextract"/"logextract.uft"
    le_uf_bin=BENCH/"src"/"logextract"/"logextract_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(le_uf_src),"-o",str(le_uf_bin)])
    results["uf_logextract"]=(rc==0,t)
    print(f"  uFlux logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_uf_src=BENCH/"src"/"analytics"/"analytics.uft"
    an_uf_bin=BENCH/"src"/"analytics"/"analytics_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(an_uf_src),"-o",str(an_uf_bin)])
    results["uf_analytics"]=(rc==0,t)
    print(f"  uFlux analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    return results
def run_benchmark(name,cmd,cwd=PROJ,timeout=120):
    print(f"  Running {name}...",end="",flush=True)
    rc,t,out,err=run_cmd(cmd,cwd=cwd,timeout=timeout)
    status="OK" if rc==0 else("TIMEOUT" if rc==-1 else f"FAIL({rc})")
    print(f" {status} {t:.2f}s")
    return{"name":name,"exit_code":rc,"time_sec":round(t,3),"status":status,
           "stdout":out[:500]if out else"","stderr":err[:200]if err else""}
def main():
    print("=== Token Counting (Qwen3-0.6B tokenizer) ===\n")
    sources={
        "Python":{
            "logextract":BENCH/"src"/"logextract"/"logextract.py",
            "analytics":BENCH/"src"/"analytics"/"analytics.py",
        },
        "Node.js":{
            "logextract":BENCH/"src"/"logextract"/"logextract.js",
            "analytics":BENCH/"src"/"analytics"/"analytics.js",
        },
        "C++":{
            "logextract":BENCH/"src"/"logextract"/"logextract.cpp",
            "analytics":BENCH/"src"/"analytics"/"analytics.cpp",
        },
        "Rust":{
            "logextract":BENCH/"src"/"logextract"/"logextract.rs",
            "analytics":BENCH/"src"/"analytics"/"analytics.rs",
        },
        "µFlux":{
            "logextract":BENCH/"src"/"logextract"/"logextract.uft",
            "analytics":BENCH/"src"/"analytics"/"analytics.uft",
        },
    }
    token_data={}
    for lang,files in sources.items():
        token_data[lang]={}
        for bench_name,path in files.items():
            tc,chars=count_tokens(path)
            lines=count_lines(path)
            token_data[lang][bench_name]={"tokens":tc,"chars":chars,"lines":lines,"path":str(path)}
            print(f"  {lang:8s} {bench_name:12s}: {tc:5d} tokens  {chars:6d} chars  {lines:3d} lines")
    compile_all()
    log_path=str(DATA/"access.log")
    csv_path=str(DATA/"sales.csv")
    print("\n=== Performance: Log Extraction (500MB access.log) ===\n")
    le_results=[]
    le_bin=BENCH/"src"/"logextract"/"logextract_cpp"
    le_results.append(run_benchmark("C++",[str(le_bin),log_path]))
    le_bin=BENCH/"src"/"logextract"/"logextract_rs"
    le_results.append(run_benchmark("Rust",[str(le_bin),log_path]))
    le_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"logextract"/"logextract.py"),log_path]))
    le_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"logextract"/"logextract.js"),log_path]))
    le_uf_bin=BENCH/"src"/"logextract"/"logextract_uf"
    le_results.append(run_benchmark("µFlux",[str(le_uf_bin),log_path],timeout=60))
    print("\n=== Performance: Data Analytics (500MB sales.csv) ===\n")
    an_results=[]
    an_bin=BENCH/"src"/"analytics"/"analytics_cpp"
    an_results.append(run_benchmark("C++",[str(an_bin),csv_path]))
    an_bin=BENCH/"src"/"analytics"/"analytics_rs"
    an_results.append(run_benchmark("Rust",[str(an_bin),csv_path]))
    an_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"analytics"/"analytics.py"),csv_path]))
    an_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"analytics"/"analytics.js"),csv_path]))
    an_uf_bin=BENCH/"src"/"analytics"/"analytics_uf"
    an_results.append(run_benchmark("µFlux",[str(an_uf_bin),csv_path],timeout=60))
    report={
        "date":time.strftime("%Y-%m-%d %H:%M:%S"),
        "data":{
            "access_log":{"size_mb":round(os.path.getsize(log_path)/1048576,1),"lines":count_lines(log_path)},
            "sales_csv":{"size_mb":round(os.path.getsize(csv_path)/1048576,1),"lines":count_lines(csv_path)},
        },
        "tokenizer":"Qwen/Qwen3-0.6B (vocab=151643)",
        "tokens":token_data,
        "performance":{"logextract":le_results,"analytics":an_results},
    }
    with open(RESULTS/"benchmark.json","w") as f:
        json.dump(report,f,indent=2)
    print(f"\nResults saved to {RESULTS/'benchmark.json'}")
    print("\n=== SUMMARY ===\n")
    print(f"{'Language':<10} {'LogExtract tokens':>18} {'Analytics tokens':>18} {'LE Time':>10} {'AN Time':>10}")
    print("-"*70)
    perf_le={r["name"]:r for r in le_results}
    perf_an={r["name"]:r for r in an_results}
    for lang in["µFlux","Rust","C++","Python","Node.js"]:
        lt=token_data[lang]["logextract"]["tokens"]
        at=token_data[lang]["analytics"]["tokens"]
        le_t=perf_le.get(lang,{}).get("time_sec","N/A")
        an_t=perf_an.get(lang,{}).get("time_sec","N/A")
        le_s=f"{le_t:.3f}s" if isinstance(le_t,float) else le_t
        an_s=f"{an_t:.3f}s" if isinstance(an_t,float) else an_t
        print(f"{lang:<10} {lt:>18} {at:>18} {le_s:>10} {an_s:>10}")
if __name__=="__main__":
    main()
