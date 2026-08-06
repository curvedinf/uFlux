#include<algorithm>
#include<array>
#include<cstdint>
#include<cstdio>
#include<cstring>
#include<string>
#include<unordered_map>
#include<utility>
#include<vector>
int main(int argc,char*argv[]){
 const char*path=argc>1?argv[1]:"bench/data/access.log";
 FILE*fp=std::fopen(path,"rb");
 if(!fp){std::fprintf(stderr,"Error: cannot open %s\n",path);return 1;}
 std::setvbuf(fp,nullptr,_IOFBF,1<<20);
 uint64_t tl=0,tb=0,s2=0,s3=0,s4=0,s5=0;
 std::array<uint64_t,24>hr{};
 std::unordered_map<std::string,uint64_t>ic;
 ic.reserve(1<<16);
 char line[8192];
 std::string ip;
 while(std::fgets(line,sizeof(line),fp)){
  char*p=line;
  char*ips=p;
  while(*p&&*p!=' ')++p;
  ip.assign(ips,static_cast<size_t>(p-ips));
  for(int i=0;i<2;++i){while(*p==' ')++p;while(*p&&*p!=' ')++p;}
  while(*p==' ')++p;
  while(*p&&*p!=':')++p;
  ++p;
  int hour=(p[0]-'0')*10+(p[1]-'0');
  while(*p&&*p!=' ')++p;
  for(int i=0;i<4;++i){while(*p==' ')++p;while(*p&&*p!=' ')++p;}
  while(*p==' ')++p;
  int status=0;
  while(*p>='0'&&*p<='9'){status=status*10+(*p-'0');++p;}
  while(*p==' ')++p;
  uint64_t nb=0;
  while(*p>='0'&&*p<='9'){nb=nb*10+static_cast<uint64_t>(*p-'0');++p;}
  ++tl;tb+=nb;
  switch(status/100){case 2:++s2;break;case 3:++s3;break;case 4:++s4;break;case 5:++s5;break;}
  if(hour>=0&&hour<24)hr[static_cast<size_t>(hour)]++;
  ++ic[ip];
 }
 std::fclose(fp);
 std::vector<std::pair<std::string,uint64_t>>top(ic.begin(),ic.end());
 std::sort(top.begin(),top.end(),[](const auto&a,const auto&b){
  if(a.second!=b.second)return a.second>b.second;
  return a.first<b.first;
 });
 std::printf("=== LOG EXTRACTION RESULTS ===\n");
 std::printf("total_lines: %llu\n",static_cast<unsigned long long>(tl));
 std::printf("total_bytes: %llu\n",static_cast<unsigned long long>(tb));
 std::printf("status_2xx: %llu\n",static_cast<unsigned long long>(s2));
 std::printf("status_3xx: %llu\n",static_cast<unsigned long long>(s3));
 std::printf("status_4xx: %llu\n",static_cast<unsigned long long>(s4));
 std::printf("status_5xx: %llu\n",static_cast<unsigned long long>(s5));
 for(int h=0;h<24;++h)
  std::printf("hour_%02d: %llu\n",h,static_cast<unsigned long long>(hr[h]));
 int n=static_cast<int>(std::min<size_t>(10,top.size()));
 for(int i=0;i<n;++i)
  std::printf("top_ip_%d: %s (%llu)\n",i+1,top[i].first.c_str(),static_cast<unsigned long long>(top[i].second));
 return 0;
}
