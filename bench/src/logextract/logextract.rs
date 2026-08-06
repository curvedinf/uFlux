use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self,BufRead,BufReader,BufWriter,Write};
fn main(){
 let path=env::args().nth(1).unwrap_or_else(||"bench/data/access.log".to_string());
 let file=File::open(&path).expect("failed to open input file");
 let mut reader=BufReader::new(file);
 let mut tl:u64=0;
 let mut tb:u64=0;
 let mut n2:u64=0;
 let mut n3:u64=0;
 let mut n4:u64=0;
 let mut n5:u64=0;
 let mut hc=[0u64;24];
 let mut ic:HashMap<String,u64>=HashMap::new();
 let mut buf=String::new();
 loop{
  buf.clear();
  if reader.read_line(&mut buf).expect("read error")==0{break;}
  let mut it=buf.split_whitespace();
  let ip=it.next().unwrap_or("");
  it.next();
  it.next();
  let ts=it.next().unwrap_or("");
  it.next();
  it.next();
  it.next();
  it.next();
  let status:u32=it.next().unwrap_or("0").parse().unwrap_or(0);
  let bytes:u64=it.next().unwrap_or("0").parse().unwrap_or(0);
  tl+=1;
  tb+=bytes;
  match status/100{2=>n2+=1,3=>n3+=1,4=>n4+=1,5=>n5+=1,_=>{}}
  let hour=ts.split(':').nth(1).and_then(|h|h.parse::<usize>().ok()).unwrap_or(0);
  if hour<24{hc[hour]+=1;}
  match ic.get_mut(ip){Some(c)=>*c+=1,None=>{ic.insert(ip.to_string(),1);}}
 }
 let mut top:Vec<(String,u64)>=ic.into_iter().collect();
 top.sort_by(|a,b|b.1.cmp(&a.1).then_with(||a.0.cmp(&b.0)));
 let stdout=io::stdout();
 let mut out=BufWriter::new(stdout.lock());
 writeln!(out,"=== LOG EXTRACTION RESULTS ===").unwrap();
 writeln!(out,"total_lines: {}",tl).unwrap();
 writeln!(out,"total_bytes: {}",tb).unwrap();
 writeln!(out,"status_2xx: {}",n2).unwrap();
 writeln!(out,"status_3xx: {}",n3).unwrap();
 writeln!(out,"status_4xx: {}",n4).unwrap();
 writeln!(out,"status_5xx: {}",n5).unwrap();
 for h in 0..24u64{writeln!(out,"hour_{:02}: {}",h,hc[h as usize]).unwrap();}
 for(i,(ip,count))in top.iter().take(10).enumerate(){writeln!(out,"top_ip_{}: {} ({})",i+1,ip,count).unwrap();}
 out.flush().unwrap();
}
