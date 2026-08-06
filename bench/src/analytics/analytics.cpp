#include<algorithm>
#include<cstdint>
#include<cstdio>
#include<cstdlib>
#include<string>
#include<unordered_map>
#include<utility>
#include<vector>
int main(int argc,char*argv[]){
 const char*path=argc>1?argv[1]:"bench/data/sales.csv";
 FILE*fp=std::fopen(path,"rb");
 if(!fp){std::fprintf(stderr,"Error: cannot open %s\n",path);return 1;}
 std::setvbuf(fp,nullptr,_IOFBF,1<<20);
 char line[8192];
 std::fgets(line,sizeof(line),fp);
 std::unordered_map<std::string,double> rr,pr,cr;
 rr.reserve(1<<16);pr.reserve(1<<16);cr.reserve(1<<16);
 uint64_t tr=0,tu=0;
 double trev=0.0;
 while(std::fgets(line,sizeof(line),fp)){
  char*p=line;
  char*fields[12]={nullptr};
  int n=0;
  fields[0]=p;
  while(*p&&n<11){if(*p==','){*p='\0';n++;fields[n]=p+1;}p++;}
  if(n<11)continue;
  const char*region=fields[2];
  const char*country=fields[3];
  const char*product=fields[4];
  uint64_t units=static_cast<uint64_t>(std::strtoll(fields[7],nullptr,10));
  double total=std::strtod(fields[10],nullptr);
  tr++;trev+=total;tu+=units;
  rr[region]+=total;
  pr[product]+=total;
  cr[country]+=total;
 }
 std::fclose(fp);
 auto cmp=[](const std::pair<std::string,double>&a,const std::pair<std::string,double>&b){
  if(a.second!=b.second)return a.second>b.second;
  return a.first<b.first;
 };
 std::vector<std::pair<std::string,double>> regions(rr.begin(),rr.end());
 std::vector<std::pair<std::string,double>> products(pr.begin(),pr.end());
 std::vector<std::pair<std::string,double>> countries(cr.begin(),cr.end());
 std::sort(regions.begin(),regions.end(),cmp);
 std::sort(products.begin(),products.end(),cmp);
 std::sort(countries.begin(),countries.end(),cmp);
 double avg=tr>0?trev/static_cast<double>(tr):0.0;
 std::printf("=== DATA ANALYTICS RESULTS ===\n");
 std::printf("total_rows: %llu\n",static_cast<unsigned long long>(tr));
 std::printf("total_revenue: %.2f\n",trev);
 std::printf("avg_order: %.2f\n",avg);
 std::printf("total_units: %llu\n",static_cast<unsigned long long>(tu));
 int nr=static_cast<int>(regions.size());
 for(int i=0;i<nr;++i)std::printf("region_%d: %s $%.2f\n",i+1,regions[i].first.c_str(),regions[i].second);
 int np=static_cast<int>(std::min<size_t>(5,products.size()));
 for(int i=0;i<np;++i)std::printf("product_%d: %s $%.2f\n",i+1,products[i].first.c_str(),products[i].second);
 int nc=static_cast<int>(std::min<size_t>(3,countries.size()));
 for(int i=0;i<nc;++i)std::printf("country_%d: %s $%.2f\n",i+1,countries[i].first.c_str(),countries[i].second);
 return 0;
}
