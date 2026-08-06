use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self,BufRead,BufReader,BufWriter,Write};
fn main(){
 let path=env::args().nth(1).unwrap_or_else(||"bench/data/sales.csv".to_string());
 let file=File::open(&path).expect("failed to open input file");
 let mut reader=BufReader::new(file);
 let mut tr:u64=0;
 let mut trev:f64=0.0;
 let mut tunits:u64=0;
 let mut rm:HashMap<String,f64>=HashMap::new();
 let mut pm:HashMap<String,f64>=HashMap::new();
 let mut cm:HashMap<String,f64>=HashMap::new();
 let mut buf=String::new();
 let mut header=true;
 loop{
  buf.clear();
  if reader.read_line(&mut buf).expect("read error")==0{break;}
  if header{header=false;continue;}
  let line=buf.trim_end_matches(|c|c=='\n'||c=='\r');
  let cols:Vec<&str>=line.split(',').collect();
  if cols.len()<12{continue;}
  let region=cols[2];
  let country=cols[3];
  let product=cols[4];
  let units:u64=cols[7].parse().unwrap_or(0);
  let total:f64=cols[10].parse().unwrap_or(0.0);
  tr+=1;
  trev+=total;
  tunits+=units;
  *rm.entry(region.to_string()).or_insert(0.0)+=total;
  *pm.entry(product.to_string()).or_insert(0.0)+=total;
  *cm.entry(country.to_string()).or_insert(0.0)+=total;
 }
 let avg=if tr>0{trev/tr as f64}else{0.0};
 let mut r:Vec<(String,f64)>=rm.into_iter().collect();
 r.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));
 let mut p:Vec<(String,f64)>=pm.into_iter().collect();
 p.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));
 let mut c:Vec<(String,f64)>=cm.into_iter().collect();
 c.sort_by(|a,b|b.1.total_cmp(&a.1).then_with(||a.0.cmp(&b.0)));
 let stdout=io::stdout();
 let mut out=BufWriter::new(stdout.lock());
 writeln!(out,"=== DATA ANALYTICS RESULTS ===").unwrap();
 writeln!(out,"total_rows: {}",tr).unwrap();
 writeln!(out,"total_revenue: {:.2}",trev).unwrap();
 writeln!(out,"avg_order: {:.2}",avg).unwrap();
 writeln!(out,"total_units: {}",tunits).unwrap();
 for(i,(n,v))in r.iter().enumerate(){writeln!(out,"region_{}: {} ${:.2}",i+1,n,v).unwrap();}
 for(i,(n,v))in p.iter().take(5).enumerate(){writeln!(out,"product_{}: {} ${:.2}",i+1,n,v).unwrap();}
 for(i,(n,v))in c.iter().take(3).enumerate(){writeln!(out,"country_{}: {} ${:.2}",i+1,n,v).unwrap();}
 out.flush().unwrap();
}
