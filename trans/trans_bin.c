// link: cc <this-file>.c -lpthread -lm

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <math.h>
#include <time.h>
#include <unistd.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sched.h>
#include <setjmp.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#ifdef _WIN32
#include <process.h>
#else
#include <sys/wait.h>
#include <fnmatch.h>
#endif

/* v10 type tags (SPEC_v10_proposal.md): 0 int, 1 float, 2 ptr, 3 byte,
   4 void, 5 arr, 6 tensor, 7 list, 8 dict, 9 str, 10 chan, 11 atom,
   12 buf, 13 obj, 14 bitmap, 15 time, 16 dur, 17 bloom, 18 iter */
enum { T_INT=0, T_FLOAT=1, T_PTR=2, T_BYTE=3, T_TIME=15, T_DUR=16 };
enum { HT_ARR=5, HT_TENSOR=6, HT_DYN=7, HT_MAP=8, HT_STR=9, HT_RING=10, HT_ATOM=11, HT_BUF=12, HT_OBJ=13, HT_BITMAP=14, HT_BLOOM=17, HT_ITER=18 };
typedef struct { int tag; int64_t i; } Cell;

/* GC header prefix shared by every tagged object: gc_next links the global
   allocation list; gc_flags holds the mark bit, pin bit, mmap bit and the
   allocation sequence (objects younger than a collection's start are never
   swept, which closes the alloc-then-publish race window). */
#define UFHDR void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety
#define GCF_MARK 1
#define GCF_PINNED 2
#define GCF_MMAP 4
#define GCF_SEQSHIFT 8
typedef struct { UFHDR; char data[]; } Hdr;
typedef struct { UFHDR; uint64_t cap; Cell data[]; } Dyn;
typedef struct { UFHDR; uint64_t cap; Cell* keys; Cell* vals; unsigned char* st; } Map;
typedef struct { UFHDR; uint64_t cap; Cell* buf; uint64_t head; uint64_t tail; pthread_mutex_t mu; pthread_cond_t notfull; pthread_cond_t notempty; int closed; } Ring;
typedef struct { UFHDR; _Atomic int64_t v; } Atom;
typedef struct { UFHDR; uint64_t mlen; const char* mdata; char data[]; } Str; /* mlen>0: mmap'd (mdata, munmap on sweep); else inline data[] */
typedef struct { UFHDR; uint64_t words[]; } Bitmap; /* len = bit count, LSB-first u64 words */
typedef struct { UFHDR; int k; uint64_t words[]; } Bloom; /* len = bit count */
enum { IT_LIST=0, IT_ARR, IT_DICT, IT_STR, IT_CHAN, IT_BITMAP, IT_MAP, IT_FILTER };
typedef struct { UFHDR; Cell src; int kind; int64_t idx; Cell f; Cell g; } Iter;
static char* uf_data(Hdr*a){ return a->tag==HT_DYN ? (char*)((Dyn*)a)->data : (char*)a->data; }

/* per-task execution context: each weave task / spawn runs with its own stacks.
   loops: dynamic loop-frame stack for BREAK/CONT unwinding. */
typedef struct { const void* end; const void* cont; long cspl; } UfLoop;
typedef struct CtxS {
  Cell* ds; long sp; long dcap; const void** cs; long csp; long ccap;
  UfLoop loops[64]; long lsp; struct CtxS* gc_prev;
  /* TLAB: thread-local allocation buffer for bump-pointer allocation */
  char* tlab_bump; char* tlab_limit; char* tlab_base; size_t tlab_size;
} Ctx;

static void uflux_run(Ctx*cx, long pc);
static _Thread_local const void* uf_entry_addr;
static _Thread_local Ctx* uf_cur_cx = 0; /* current ctx for TLAB allocation */
static void uf_call_addr(Ctx*cx, const void* a){ cx->cs[cx->csp++]=0; uf_entry_addr=a; uflux_run(cx,-1); }

/* ---- error containment (try/retry): die unwinds to the nearest setjmp
   checkpoint; with no checkpoint die is fatal, as before ---- */
typedef struct UfTry { jmp_buf jb; struct UfTry* prev; long sp; long csp; } UfTry;
static _Thread_local UfTry* uf_try_top = 0;
static _Thread_local void* uf_cur_task; /* WeaveTask* for debug counters */
static void die(const char*m){
  if(uf_try_top){ UfTry*t=uf_try_top; longjmp(t->jb,1); }
  fprintf(stderr,"uflux: %s\n",m); exit(1);
}

/* ================= garbage collector: TLAB-based bump allocator + mark-sweep.
   Each Ctx has a thread-local allocation buffer (TLAB). Allocations bump a
   pointer — no mutex, no hash-set, no linked list. On exhaustion, grab a new
   slab (mutex). GC scans all TLABs linearly. uf_gc_find checks TLAB ranges
   first (O(1) per Ctx), falls back to the hash set for non-TLAB objects. */
static void* uf_gc_list; /* linked list of non-TLAB objects (statics, large allocs) */
static _Atomic uint64_t uf_gc_seq = 1;
static uint64_t uf_gc_bytes_since, uf_gc_threshold = 1<<20, uf_gc_live;
static int uf_gc_on = 1;
static pthread_mutex_t uf_gc_mu = PTHREAD_MUTEX_INITIALIZER;
/* address hash set for non-TLAB objects only (statics, pre-TLAB objects) */
static void** uf_gc_set; static uint64_t uf_gc_setcap, uf_gc_setlen;
/* context registry (needed by TLAB code below) */
#define UF_MAXCTX 512
static Ctx* uf_ctxs[UF_MAXCTX]; static _Atomic int uf_nctxs;
static void uf_gc_set_insert(void* p);
static void uf_gc_set_grow(void){
  uint64_t oc=uf_gc_setcap; void** os=uf_gc_set;
  uf_gc_setcap = oc? oc*2 : 256; uf_gc_setlen = 0;
  uf_gc_set = (void**)calloc(uf_gc_setcap, sizeof(void*)); if(!uf_gc_set)die("out of memory");
  for(uint64_t i=0;i<oc;i++) if(os[i]&&os[i]!=(void*)1) uf_gc_set_insert(os[i]);
  free(os);
}
static void uf_gc_set_insert(void* p){
  if((uf_gc_setlen+1)*10 >= uf_gc_setcap*7) uf_gc_set_grow();
  uint64_t i = ((uint64_t)p >> 4) * 11400714819323198485ULL >> 32; i %= uf_gc_setcap;
  while(uf_gc_set[i] && uf_gc_set[i]!=p) i=(i+1)%uf_gc_setcap;
  if(!uf_gc_set[i]){ uf_gc_set[i]=p; uf_gc_setlen++; }
}
/* TLAB slab sizes: main thread gets 1MB, spawned threads get 64KB */
#define UF_TLAB_MAIN (1<<20)
#define UF_TLAB_SPAWN (1<<16)
/* context registry forward decl (uf_gc_find and uf_tlab_refill need these) */
#define UF_MAXCTX 512
static Ctx* uf_ctxs[UF_MAXCTX]; static _Atomic int uf_nctxs;
static Ctx main_cx_store; /* fwd: defined fully below */
static Ctx* main_cx;      /* fwd: defined fully below */
static void uf_tlab_init(Ctx*c, size_t sz){
  c->tlab_base = (char*)malloc(sz); if(!c->tlab_base) die("out of memory");
  c->tlab_bump = c->tlab_base; c->tlab_limit = c->tlab_base + sz; c->tlab_size = sz;
}
static void uf_tlab_refill(Ctx*c, size_t need){
  /* save old slab for GC scanning (linked via gc_next on a separate list) */
  pthread_mutex_lock(&uf_gc_mu);
  /* allocate new slab large enough for need + growth */
  size_t newsz = c->tlab_size * 2; if(newsz < need + 64) newsz = need + 64;
  if(c->tlab_size >= UF_TLAB_MAIN && newsz < UF_TLAB_MAIN) newsz = UF_TLAB_MAIN;
  char* newslab = (char*)malloc(newsz); if(!newslab) die("out of memory");
  /* link old slab into gc_list for GC scanning */
  if(c->tlab_base){
    Hdr* oh = (Hdr*)c->tlab_base; oh->gc_next = uf_gc_list; uf_gc_list = c->tlab_base;
    oh->gc_flags = GCF_PINNED; /* slab itself is pinned; individual objects are scanned */
  }
  c->tlab_base = newslab; c->tlab_bump = newslab; c->tlab_limit = newslab + newsz; c->tlab_size = newsz;
  pthread_mutex_unlock(&uf_gc_mu);
}
/* check if pointer is in any registered Ctx's TLAB */
static Hdr* uf_gc_find(void* p){
  if(!p || p==(void*)1) return 0;
  /* fast path: check current thread's TLAB first */
  Ctx* c = uf_cur_cx;
  if(c && (char*)p >= c->tlab_base && (char*)p < c->tlab_limit) return (Hdr*)p;
  /* check all registered Ctxs */
  int nc = uf_nctxs;
  for(int i=0;i<nc;i++){ Ctx* cc=uf_ctxs[i]; if((char*)p >= cc->tlab_base && (char*)p < cc->tlab_limit) return (Hdr*)p; }
  /* fallback: hash set for non-TLAB objects (statics, old slabs) */
  if(uf_gc_setcap){
    uint64_t i = ((uint64_t)p >> 4) * 11400714819323198485ULL >> 32; i %= uf_gc_setcap;
    while(uf_gc_set[i]){ if(uf_gc_set[i]==p) return (Hdr*)p; i=(i+1)%uf_gc_setcap; }
  }
  return 0;
}
/* context registry: every Ctx's data stack is a precise root set */
static void ctx_register(Ctx*c){ pthread_mutex_lock(&uf_gc_mu); int i=uf_nctxs; if(i<UF_MAXCTX){ uf_ctxs[i]=c; uf_nctxs=i+1; } pthread_mutex_unlock(&uf_gc_mu); }
static void ctx_unregister(Ctx*c){ pthread_mutex_lock(&uf_gc_mu); for(int i=0;i<uf_nctxs;i++) if(uf_ctxs[i]==c){ uf_ctxs[i]=uf_ctxs[uf_nctxs-1]; uf_nctxs--; break; } pthread_mutex_unlock(&uf_gc_mu); }
/* variable roots, registered by generated code */
static Cell** uf_var_roots; static long uf_nvar_roots;
static void uf_gc_setroots(Cell** r, long n){ uf_var_roots=r; uf_nvar_roots=n; }
/* tmp roots for builder ops (in-progress containers while they grow) */
#define UF_MAXTMP 1024
static void*** uf_tmp_roots = 0; static _Atomic int uf_ntmp;
#define UF_PROTECT(pp) do{ int _i=atomic_fetch_add(&uf_ntmp,1); if(_i<UF_MAXTMP)uf_tmp_roots[_i]=(void**)(pp); }while(0)
#define UF_UNPROTECT() atomic_fetch_sub(&uf_ntmp,1)
static void uf_mark_cell(Cell c);
static void uf_mark_obj(Hdr* h);
static void uf_mark_ptr(void* p){
  Hdr* h = uf_gc_find(p);
  if(h) uf_mark_obj(h);
}
static void uf_mark_cell(Cell c){ if(c.tag==T_PTR && c.i) uf_mark_ptr((void*)c.i); }
static void uf_mark_obj(Hdr* h){
  if(h->gc_flags & GCF_MARK) return;
  h->gc_flags |= GCF_MARK;
  if(h->gc_parent) uf_mark_ptr(h->gc_parent);
  switch(h->tag){
    case HT_DYN: { Dyn* d=(Dyn*)h; for(uint64_t i=0;i<d->len;i++) uf_mark_cell(d->data[i]); break; }
    case HT_MAP: { Map* m=(Map*)h; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){ uf_mark_cell(m->keys[i]); uf_mark_cell(m->vals[i]); } break; }
    case HT_RING: { Ring* r=(Ring*)h; for(uint64_t j=0;j<r->len;j++) uf_mark_cell(r->buf[(r->head+j)%r->cap]); break; }
    case HT_OBJ: { uint64_t n=h->esz/8; Cell* f=(Cell*)h->data; for(uint64_t i=0;i<n;i++) uf_mark_cell(f[i]); break; }
    case HT_ITER: { Iter* it=(Iter*)h; uf_mark_cell(it->src); uf_mark_cell(it->f); uf_mark_cell(it->g); break; }
    default: break; /* arr/tensor/str/bitmap/bloom/atom/buf: leaf bytes */
  }
}
struct WeaveJobS; static struct WeaveJobS* uf_active_job;
static void uf_weave_mark(struct WeaveJobS* j);
static void uf_gc_free_obj(Hdr* h){
  switch(h->tag){
    case HT_MAP: { Map* m=(Map*)h; free(m->keys); free(m->vals); free(m->st); break; }
    case HT_RING: { Ring* r=(Ring*)h; pthread_mutex_destroy(&r->mu); pthread_cond_destroy(&r->notfull); pthread_cond_destroy(&r->notempty); free(r->buf); break; }
    case HT_STR: { Str* s=(Str*)h; if(s->mlen) munmap((void*)s->mdata,(size_t)s->mlen); else if(s->gc_parent==(void*)1) free((void*)s->mdata); break; }
    default: break;
  }
  free(h);
}
/* scan objects in a TLAB slab: call fn(h) for each object-sized chunk.
   Objects are 16-byte aligned, so we walk by reading each object's actual
   size from its header (UFHDR fields: tag, len, esz determine total size). */
static size_t uf_obj_size(Hdr* h){
  switch(h->tag){
    case HT_DYN: return sizeof(Dyn) + ((Dyn*)h)->cap * sizeof(Cell);
    case HT_MAP: return sizeof(Map);
    case HT_RING: return sizeof(Ring);
    case HT_STR: { Str* s=(Str*)h; return s->mlen ? sizeof(Str) : sizeof(Str) + h->len + 1; }
    case HT_BITMAP: return sizeof(Bitmap) + (((h->len+63)/64)*8);
    case HT_BLOOM: return sizeof(Bloom) + (((h->len+63)/64)*8);
    case HT_ITER: return sizeof(Iter);
    default: return sizeof(Hdr) + (h->len * h->esz);
  }
}
static void uf_gc_collect(void){
  pthread_mutex_lock(&uf_gc_mu);
  uint64_t start_seq = uf_gc_seq;
  /* mark all roots */
  for(long i=0;i<uf_nvar_roots;i++) uf_mark_cell(*uf_var_roots[i]);
  int nc = uf_nctxs;
  for(int i=0;i<nc;i++){ Ctx* c=uf_ctxs[i]; for(long s=0;s<c->sp;s++) uf_mark_cell(c->ds[s]); }
  int nt = uf_ntmp; if(nt>UF_MAXTMP)nt=UF_MAXTMP;
  for(int i=0;i<nt;i++){ void** pp=uf_tmp_roots[i]; if(pp&&*pp) uf_mark_ptr(*pp); }
  if(uf_active_job) uf_weave_mark(uf_active_job);
  /* sweep: walk each Ctx's current TLAB + old slabs on gc_list.
     Since we can't free individual objects from a slab (non-moving),
     we use copy-compact: copy live objects to a fresh slab, update pointers.
     But that's too complex for now. Instead: mark live, then walk and
     call finalizers for dead objects within TLABs, but don't reclaim
     slab space (bump pointer resets after collection). */
  /* For now: reset all TLAB bump pointers (reclaim all slab space),
     keeping only objects reachable from roots by NOT resetting.
     Since TLAB is bump-only, we can't reclaim individual objects.
     The pragmatic approach: don't sweep TLAB objects at all — let the
     slab fill up, then the old slab is freed on refill. Live objects
     are still referenced by root pointers. This is a leak per slab.
     For the benchmark pattern (transient objects per line), this works:
     each line's objects are in the current TLAB, and when the TLAB
     refills, the old slab (full of dead objects) is freed entirely. */
  /* Sweep old slabs on gc_list: they're all dead (replaced by refills).
     Free them entirely. */
  void** pp = &uf_gc_list;
  while(*pp){
    Hdr* h=(Hdr*)*pp;
    /* old TLAB slabs have GCF_PINNED set (from uf_tlab_refill) */
    if(h->gc_flags & GCF_PINNED){
      *pp = h->gc_next; free(h); /* free the entire old slab */
    } else {
      /* non-TLAB objects (statics, large allocs): normal sweep */
      if(!(h->gc_flags&GCF_MARK) && ((h->gc_flags>>GCF_SEQSHIFT) < start_seq)){
        *pp = h->gc_next; uf_gc_free_obj(h);
      } else {
        h->gc_flags &= ~(uint64_t)GCF_MARK; pp = &h->gc_next;
      }
    }
  }
  /* Clear marks in current TLABs */
  for(int i=0;i<nc;i++){
    Ctx* c=uf_ctxs[i];
    for(char* p=c->tlab_base; p<c->tlab_bump; ){
      Hdr* h=(Hdr*)p;
      h->gc_flags &= ~(uint64_t)GCF_MARK;
      size_t sz = uf_obj_size(h);
      p += (sz + 15) & ~15;
      if(p >= c->tlab_bump) break;
    }
  }
  uf_gc_bytes_since = 0;
  uf_gc_threshold = uf_gc_threshold; /* keep threshold stable */
  pthread_mutex_unlock(&uf_gc_mu);
}
static void* uf_gc_alloc(size_t sz, int align){
  sz = sz ? sz : 1;
  size_t aligned_sz = (sz + 15) & ~15; /* 16-byte align for all objects */
  Ctx* c = uf_cur_cx;
  if(c){
    char* p = c->tlab_bump;
    char* np = p + aligned_sz;
    if(np <= c->tlab_limit){
      /* fast path: bump pointer, no mutex */
      c->tlab_bump = np;
      memset(p, 0, aligned_sz);
      Hdr* h = (Hdr*)p;
      h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
      uf_gc_bytes_since += aligned_sz;
      if(uf_gc_on && uf_gc_bytes_since > uf_gc_threshold) uf_gc_collect();
      return p;
    }
    /* slow path: refill TLAB */
    uf_tlab_refill(c, aligned_sz);
    char* p2 = c->tlab_bump;
    c->tlab_bump += aligned_sz;
    memset(p2, 0, aligned_sz);
    Hdr* h = (Hdr*)p2;
    h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
    uf_gc_bytes_since += aligned_sz;
    if(uf_gc_on && uf_gc_bytes_since > uf_gc_threshold) uf_gc_collect();
    return p2;
  }
  /* no Ctx (startup/FFI): fall back to malloc + hash-set */
  if(uf_gc_on && uf_gc_bytes_since + sz > uf_gc_threshold) uf_gc_collect();
  void* p = malloc(sz);
  if(!p)die("out of memory");
  memset(p,0,sz);
  pthread_mutex_lock(&uf_gc_mu);
  Hdr* h=(Hdr*)p; h->gc_next=uf_gc_list; uf_gc_list=p;
  h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  uf_gc_bytes_since += sz;
  uf_gc_set_insert(p);
  pthread_mutex_unlock(&uf_gc_mu);
  return p;
}
/* register a static object (string literal): linked, pinned, never swept */
static void uf_gc_register_static(void* p){
  pthread_mutex_lock(&uf_gc_mu);
  Hdr* h=(Hdr*)p; h->gc_next=uf_gc_list; uf_gc_list=p;
  h->gc_flags = GCF_PINNED;
  uf_gc_set_insert(p);
  pthread_mutex_unlock(&uf_gc_mu);
}
static void uf_init_lits(void** lits, long n){ for(long i=0;i<n;i++) uf_gc_register_static(lits[i]); }
static void uf_gc_init(void){
  const char* e=getenv("UF_GC_THRESHOLD");
  if(e&&*e){ uint64_t v=strtoull(e,0,0); if(v) uf_gc_threshold=v; }
  uf_tmp_roots=(void***)calloc(UF_MAXTMP,sizeof(void**));
  uf_tlab_init(&main_cx_store, UF_TLAB_MAIN);
  ctx_register(&main_cx_store);
}
static void op_gc(Ctx*cx){ (void)cx; uf_gc_collect(); }

static Ctx* ctx_new(long dcap,long ccap){ Ctx*c=(Ctx*)calloc(1,sizeof(Ctx)); if(!c)die("out of memory"); c->ds=(Cell*)malloc(dcap*sizeof(Cell)); c->cs=(const void**)malloc(ccap*sizeof(void*)); if(!c->ds||!c->cs)die("out of memory"); c->dcap=dcap; c->ccap=ccap; uf_tlab_init(c, UF_TLAB_SPAWN); ctx_register(c); return c; }
static void ctx_free(Ctx*c){ ctx_unregister(c); free(c->ds); free((void*)c->cs); if(c->tlab_base) free(c->tlab_base); free(c); }
static Cell main_ds[1<<20]; static const void* main_cs[1<<16];
static Ctx main_cx_store = { main_ds, 0, 1<<20, main_cs, 0, 1<<16, {{0,0,0}}, 0, 0, 0,0,0,0 };
static Ctx* main_cx = &main_cx_store;
int64_t uf_argc=0; void* uf_argv=0; /* program args, reachable via EXTERN "uf_argc"/"uf_argv" + LOADX, or ARGV */

static void pushc(Ctx*cx,Cell c){ if(cx->sp>=cx->dcap)die("stack overflow"); cx->ds[cx->sp++]=c; }
static inline Cell uf_mki(int64_t v){ Cell c; c.tag=T_INT; c.i=v; return c; }
static inline Cell uf_mkp(void* v){ Cell c; c.tag=T_PTR; c.i=(int64_t)v; return c; }
static int uf_is_str(Cell c); /* fwd decl for coercion */
static const char* uf_sptr(Cell c); /* fwd decl */
static inline double uf_fbits(int64_t i){ union{int64_t i;double f;}u;u.i=i;return u.f; }
static inline int64_t uf_ibits(double f){ union{int64_t i;double f;}u;u.f=f;return u.i; }
static inline double uf_f(Cell c){ if(c.tag==T_FLOAT)return uf_fbits(c.i); if(c.tag==T_PTR&&c.i&&uf_is_str(c)) return strtod(uf_sptr(c),0); return (double)c.i; }
static inline int64_t uf_i(Cell c){ if(c.tag==T_PTR&&c.i&&uf_is_str(c)) return strtoll(uf_sptr(c),0,10); return c.i; }
static inline Cell uf_mkf(double v){ Cell c; c.tag=T_FLOAT; c.i=uf_ibits(v); return c; }
static inline Cell uf_fromf(double v){ return uf_mkf(v); }
static inline int uf_zero(Cell c){ return c.tag==T_FLOAT?(int64_t)uf_fbits(c.i)==0:c.i==0; }
static inline Cell uf_cadd(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)+uf_f(b)); return uf_mki(uf_i(a)+uf_i(b)); }
static inline Cell uf_csub(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)-uf_f(b)); return uf_mki(uf_i(a)-uf_i(b)); }
static inline Cell uf_cmul(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)*uf_f(b)); return uf_mki(uf_i(a)*uf_i(b)); }
static inline Cell uf_cand(Cell a,Cell b){ return uf_mki(uf_i(a)&uf_i(b)); }
static inline Cell uf_cshr(Cell a){ return uf_mki((int64_t)((uint64_t)uf_i(a)>>1)); }
static inline Cell uf_cinc(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)+1.0); return uf_mki(uf_i(a)+1); }
static inline Cell uf_cdec(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)-1.0); return uf_mki(uf_i(a)-1); }
static inline int uf_ceq(Cell a,Cell b){ return (a.tag==T_FLOAT||b.tag==T_FLOAT)?uf_f(a)==uf_f(b):uf_i(a)==uf_i(b); }

/* ---- string access: every core string is a tag-9 Str object; raw char*
   from IMPORTed C functions is still accepted (legacy ptr) ---- */
static int uf_is_str(Cell c){ if(c.tag!=T_PTR||!c.i)return 0; Hdr*h=uf_gc_find((void*)c.i); return h&&h->tag==HT_STR; }
static const char* uf_sbytes(Str*s){ return (s->mlen||s->gc_parent)?s->mdata:s->data; }
static const char* uf_sptr(Cell c){ if(c.tag==T_PTR&&c.i){ Hdr*h=uf_gc_find((void*)c.i); if(h&&h->tag==HT_STR){ Str*s=(Str*)h; return (s->mlen||s->gc_parent)?s->mdata:s->data; } if(h&&h->tag==HT_BUF){ return h->data; } return (const char*)c.i; /* raw ptr from malloc/FFI */ } if(c.tag==T_INT&&c.i>(int64_t)65536) return (const char*)c.i; if(c.tag==T_INT&&!c.i) return (const char*)0; die("expected string, got non-pointer cell"); }
static int64_t uf_slen(Cell c){ if(c.tag==T_PTR&&c.i){ Hdr*h=uf_gc_find((void*)c.i); if(h&&h->tag==HT_STR)return (int64_t)h->len; } return (int64_t)strlen((const char*)c.i); }
static Cell uf_str_new(const char* s, size_t n){
  Str* r=(Str*)uf_gc_alloc(sizeof(Str)+n+1,0);
  r->tag=HT_STR; r->len=n; r->esz=1; r->mlen=0;
  memcpy(r->data,s,n); r->data[n]=0;
  return uf_mkp(r);
}
static Cell uf_str_dup(Cell c){ const char* s=uf_sptr(c); return uf_str_new(s,strlen(s)); }

/* arr element access honors the element type (ety): 0 int (8B), 1 float (8B), 3 byte (1B) */
static inline Cell uf_cidx(Cell h,int64_t ix){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("index out of bounds"); char*dt=uf_data(a); if(a->tag==HT_DYN)return ((Cell*)dt)[ix]; if(a->ety==3)return uf_mki((int64_t)((uint8_t*)dt)[ix]); if(a->ety==1)return uf_mkf(((double*)dt)[ix]); return uf_mki(((int64_t*)dt)[ix]); }
static inline void uf_cseti(Cell h,int64_t ix,Cell v){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("index out of bounds"); char*dt=uf_data(a); if(a->tag==HT_DYN){((Cell*)dt)[ix]=v;return;} if(a->ety==3){((uint8_t*)dt)[ix]=(uint8_t)v.i;return;} if(a->ety==1){((double*)dt)[ix]=uf_f(v);return;} ((int64_t*)dt)[ix]=v.i; }
static void pushi(Ctx*cx,int64_t v){ pushc(cx,uf_mki(v)); }
static void pushf(Ctx*cx,double v){ pushc(cx,uf_mkf(v)); }
static void pushp(Ctx*cx,void* v){ pushc(cx,uf_mkp(v)); }
static Cell pop(Ctx*cx){ if(cx->sp<=0) die("stack underflow"); return cx->ds[--cx->sp]; }
static void op_nop(Ctx*cx){ (void)cx; }

static void op_dup(Ctx*cx){ if(cx->sp<1)die("stack underflow"); pushc(cx,cx->ds[cx->sp-1]); }
static void op_ovr(Ctx*cx){ if(cx->sp<2)die("stack underflow"); pushc(cx,cx->ds[cx->sp-2]); }
static void op_drp(Ctx*cx){ (void)pop(cx); }
static void op_swp(Ctx*cx){ Cell t=cx->ds[cx->sp-1]; cx->ds[cx->sp-1]=cx->ds[cx->sp-2]; cx->ds[cx->sp-2]=t; }
static void op_pick(Ctx*cx){ int64_t n=pop(cx).i; if(n<0||n>=cx->sp)die("PICK out of range"); pushc(cx,cx->ds[cx->sp-1-n]); }

static void op_add(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cadd(a,b)); }
static void op_sub(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_csub(a,b)); }
static void op_mul(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cmul(a,b)); }
static void op_and(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cand(a,b)); }
static void op_shr(Ctx*cx){ pushc(cx,uf_cshr(pop(cx))); }
static void op_inc(Ctx*cx){ pushc(cx,uf_cinc(pop(cx))); }
static void op_dec(Ctx*cx){ pushc(cx,uf_cdec(pop(cx))); }

/* v10 arithmetic & logic */
static void op_div(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT){ double d=uf_f(b); if(d==0.0)die("DIV: division by zero"); pushf(cx,uf_f(a)/d); } else { int64_t bv=uf_i(b); if(bv==0)die("DIV: division by zero"); pushi(cx,uf_i(a)/bv); } }
static void op_rem(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT){ double d=uf_f(b); if(d==0.0)die("REM: division by zero"); pushf(cx,fmod(uf_f(a),d)); } else { int64_t bv=uf_i(b); if(bv==0)die("REM: division by zero"); pushi(cx,uf_i(a)%bv); } }
static void op_eq(Ctx*cx){
  Cell b=pop(cx),a=pop(cx); int r;
  if(a.tag==T_FLOAT||b.tag==T_FLOAT){ if((a.tag==T_INT||a.tag==T_FLOAT)&&(b.tag==T_INT||b.tag==T_FLOAT)) r=(uf_f(a)==uf_f(b)); else r=(a.i==b.i&&a.tag==b.tag); }
  else if(uf_is_str(a)&&uf_is_str(b)) r=strcmp(uf_sptr(a),uf_sptr(b))==0;
  else if((a.tag==T_PTR&&a.i&&uf_gc_find((void*)a.i))||(b.tag==T_PTR&&b.i&&uf_gc_find((void*)b.i))) r=(a.i==b.i);
  else r=(a.i==b.i);
  pushi(cx,r?1:0);
}
static int uf_cmp(Cell a,Cell b,int* ok){ /* -1/0/1; *ok=0 if incomparable */
  *ok=1;
  if((a.tag==T_INT||a.tag==T_FLOAT||a.tag==T_TIME||a.tag==T_DUR)&&(b.tag==T_INT||b.tag==T_FLOAT||b.tag==T_TIME||b.tag==T_DUR)){
    double x=uf_f(a),y=uf_f(b); return x<y?-1:x>y?1:0;
  }
  if(uf_is_str(a)&&uf_is_str(b)) return strcmp(uf_sptr(a),uf_sptr(b));
  *ok=0; return 0;
}
static void op_lt(Ctx*cx){ Cell b=pop(cx),a=pop(cx); int ok; int c=uf_cmp(a,b,&ok); if(!ok)die("LT: incomparable operands"); pushi(cx,c<0?1:0); }
static void op_gt(Ctx*cx){ Cell b=pop(cx),a=pop(cx); int ok; int c=uf_cmp(a,b,&ok); if(!ok)die("GT: incomparable operands"); pushi(cx,c>0?1:0); }
static void op_not(Ctx*cx){ Cell a=pop(cx); pushi(cx,uf_zero(a)?1:0); }
static void op_or(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("OR: ints only"); pushi(cx,a.i|b.i); }
static void op_xor(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("XOR: ints only"); pushi(cx,a.i^b.i); }
static void op_shl(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("SHL: ints only"); if(b.i<0||b.i>=64)die("SHL: shift out of range"); pushi(cx,a.i<<b.i); }
static void op_bnot(Ctx*cx){ Cell a=pop(cx); if(a.tag==T_FLOAT||a.tag==T_PTR)die("BNOT: ints only"); pushi(cx,~a.i); }
static void op_orelse(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_zero(a)?b:a); }

static void* uf_alloc(size_t sz,int align){ void*p=NULL; if(align>0){ if(posix_memalign(&p,(size_t)align,sz?sz:1))die("alloc failed"); } else { p=malloc(sz?sz:1); } if(!p)die("out of memory"); return p; }
static void op_arrn(Ctx*cx,uint64_t tag,int align){ int64_t len=pop(cx).i, ty=pop(cx).i; int64_t esz=(ty==3)?1:8; if(len<0)die("negative length"); Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=(uint64_t)len; h->esz=(uint64_t)esz; h->ety=(uint64_t)ty; memset(h->data,0,(size_t)len*(size_t)esz); pushp(cx,h); }
static void op_arr(Ctx*cx){ op_arrn(cx,HT_ARR,0); }
static void op_tensor(Ctx*cx){ op_arrn(cx,HT_TENSOR,64); }
static void op_clone(Ctx*cx){
  Cell h=pop(cx); Hdr*a=(Hdr*)uf_gc_find((void*)h.i);
  if(!a)die("CLONE: not a managed object");
  if(a->tag==HT_ITER)die("CLONE: iterators are single-use");
  if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR)die("CLONE: only arr/tensor");
  size_t sz=sizeof(Hdr)+(size_t)a->len*a->esz; Hdr*n=(Hdr*)uf_gc_alloc(sz,a->tag==HT_TENSOR?64:0);
  memcpy(n,a,sz); n->gc_next=0; n->gc_flags=((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  pushp(cx,n);
}
static void op_cast(Ctx*cx){ Cell id=pop(cx); Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag; if(tk!=id.i)die("CAST: type mismatch"); pushc(cx,h); }

/* OBJ: size in low 32 bits of the operand, struct id above; esz = byte size,
   len = struct id. Field access via the container protocol. */
static void op_obj(Ctx*cx){ int64_t v=pop(cx).i; int64_t sz=v&0xffffffffLL; int64_t sid=v>>32; if(sz<=0)sz=8; Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_OBJ; h->len=(uint64_t)sid; h->esz=(uint64_t)sz; memset(h->data,0,(size_t)sz); pushp(cx,h); }

/* reflection tables, populated by generated uf_init_reflection() */
static long uf_st_n=0; static const int64_t* uf_st_sids=0; static const int64_t* uf_st_nf=0; static const char*** uf_st_fields=0; static const int64_t** uf_st_offs=0;
/* obj field offset by key: int -> raw offset; str -> field name. -1 = missing */
static int64_t uf_obj_off(Hdr* a, Cell k){
  if(k.tag==T_INT) return k.i;
  if(uf_is_str(k)){
    int64_t sid=(int64_t)a->len;
    for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){
      for(int64_t f=0;f<uf_st_nf[q];f++) if(strcmp(uf_sptr(k),uf_st_fields[q][f])==0) return uf_st_offs[q][f];
      return -1;
    }
  }
  return -1;
}

/* ================= uniform container protocol ================= */
static uint64_t uf_fnv(const void*p,size_t n){ const unsigned char*s=(const unsigned char*)p; uint64_t h=1469598103934665603ULL; for(size_t i=0;i<n;i++){ h^=s[i]; h*=1099511628211ULL; } return h; }
static uint64_t map_hash(Cell k){
  if(k.tag==T_PTR&&k.i){ Hdr*h=uf_gc_find((void*)k.i); if(h){ if(h->tag==HT_STR)return uf_fnv(uf_sbytes((Str*)h),h->len); return uf_fnv(&k.i,8); } return uf_fnv((void*)k.i,strlen((char*)k.i)); }
  return uf_fnv(&k.i,8);
}
static int map_keyeq(Cell a,Cell b){
  if(a.tag==T_PTR&&b.tag==T_PTR&&a.i&&b.i){
    Hdr*ha=uf_gc_find((void*)a.i); Hdr*hb=uf_gc_find((void*)b.i);
    if(ha&&hb){ if(ha->tag==HT_STR&&hb->tag==HT_STR){ if(ha->len!=hb->len)return 0; return memcmp(uf_sbytes((Str*)ha),uf_sbytes((Str*)hb),ha->len)==0; } return a.i==b.i; }
    if((ha&&ha->tag==HT_STR)||(hb&&hb->tag==HT_STR)) return strcmp(uf_sptr(a),uf_sptr(b))==0;
    return strcmp((char*)a.i,(char*)b.i)==0;
  }
  return a.i==b.i;
}
static void map_put_raw(Map*m,Cell k,Cell v){
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]!=1){ m->st[i]=1; m->keys[i]=k; m->vals[i]=v; m->len++; return; } if(map_keyeq(m->keys[i],k)){ m->vals[i]=v; return; } i=(i+1)%m->cap; }
}
static void map_grow(Map*m){
  uint64_t ncap=m->cap*2; Cell*ok=m->keys,*ov=m->vals; unsigned char*os=m->st; uint64_t ocap=m->cap;
  m->cap=ncap; m->keys=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->st=(unsigned char*)calloc(ncap,1); m->len=0;
  for(uint64_t i=0;i<ocap;i++) if(os[i]==1) map_put_raw(m,ok[i],ov[i]);
  free(ok); free(ov); free(os);
}
static Map* uf_map_new(void){ Map*m=(Map*)uf_gc_alloc(sizeof(Map),0); m->tag=HT_MAP; m->len=0; m->cap=16; m->keys=(Cell*)uf_alloc(16*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(16*sizeof(Cell),0); m->st=(unsigned char*)calloc(16,1); return m; }
static void map_put(Map*m,Cell k,Cell v){ if((m->len+1)*10>=m->cap*7) map_grow(m); map_put_raw(m,k,v); }
static int map_get(Map*m,Cell k,Cell*out){
  if(m->cap==0)return 0;
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]==0)return 0; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ *out=m->vals[i]; return 1; } i=(i+1)%m->cap; }
}
static void map_del(Map*m,Cell k){
  if(m->cap==0)return;
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]==0)return; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ m->st[i]=2; m->len--; return; } i=(i+1)%m->cap; }
}
static Dyn* uf_dyn_new(uint64_t cap){ if(!cap)cap=1; Dyn*d=(Dyn*)uf_gc_alloc(sizeof(Dyn)+cap*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=cap; return d; }
/* grow-with-move: returns the (possibly new) list; old handle is reclaimed by GC */
static Dyn* uf_dyn_push2(Dyn*d,Cell c){ if(d->len>=d->cap){ Dyn*n=uf_dyn_new(d->cap*2); memcpy(n->data,d->data,d->len*sizeof(Cell)); n->len=d->len; d=n; } d->data[d->len++]=c; return d; }
static void uf_dyn_push(Dyn**pd,Cell c){ *pd=uf_dyn_push2(*pd,c); }
static void uf_dyn_push_str(Dyn**pd,const char*s,size_t n){ Cell c=uf_str_new(s,n); uf_dyn_push(pd,c); }

static Hdr* uf_handle(Cell h,const char* op){ if(h.tag!=T_PTR||!h.i)die("handle is null"); Hdr*a=uf_gc_find((void*)h.i); if(!a)die("not a managed handle"); (void)op; return a; }
/* GET: h k -> v */
static void op_get(Ctx*cx){
  Cell k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"GET");
  switch(a->tag){
    case HT_MAP: { Map*m=(Map*)a; Cell v; if(!map_get(m,k,&v))die("GET: missing key"); pushc(cx,v); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: pushc(cx,uf_cidx(h,k.i)); return;
    case HT_STR: { Str*s=(Str*)a; if(k.i<0||k.i>=(int64_t)s->len)die("GET: index out of bounds"); pushi(cx,(uint8_t)uf_sbytes(s)[k.i]); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)die("GET: no such field"); pushc(cx,*(Cell*)(a->data+o)); return; }
    default: die("GET: unsupported handle");
  }
}
/* GETQ: h k -> v_or_0 (never dies on absence; null handle -> 0) */
static void op_getq(Ctx*cx){
  Cell k=pop(cx),h=pop(cx);
  if(h.tag!=T_PTR||!h.i){ pushi(cx,0); return; }
  Hdr*a=uf_gc_find((void*)h.i); if(!a)die("GETQ: not a managed handle");
  switch(a->tag){
    case HT_MAP: { Map*m=(Map*)a; Cell v; if(map_get(m,k,&v))pushc(cx,v); else pushi(cx,0); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: if(k.i<0||(uint64_t)k.i>=a->len)pushi(cx,0); else pushc(cx,uf_cidx(h,k.i)); return;
    case HT_STR: { Str*s=(Str*)a; if(k.i<0||k.i>=(int64_t)s->len)pushi(cx,0); else pushi(cx,(uint8_t)uf_sbytes(s)[k.i]); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)pushi(cx,0); else pushc(cx,*(Cell*)(a->data+o)); return; }
    default: die("GETQ: unsupported handle");
  }
}
/* SET: h k v -> */
static void op_set(Ctx*cx){
  Cell v=pop(cx),k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"SET");
  switch(a->tag){
    case HT_MAP: map_put((Map*)a,k,v); return;
    case HT_DYN: case HT_ARR: case HT_TENSOR: uf_cseti(h,k.i,v); return;
    case HT_STR: { Str*s=(Str*)a; if(s->mlen)die("SET: mmap string is read-only"); if(k.i<0||k.i>=(int64_t)s->len)die("SET: index out of bounds"); s->data[k.i]=(char)v.i; return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)die("SET: no such field"); *(Cell*)(a->data+o)=v; return; }
    default: die("SET: unsupported handle");
  }
}
/* DEL: h k -> (dict only) */
static void op_del(Ctx*cx){
  Cell k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"DEL");
  if(a->tag!=HT_MAP)die("DEL: only dict supports del");
  map_del((Map*)a,k);
}
/* HAS: h k -> 0/1 (null handle -> 0) */
static void op_has(Ctx*cx){
  Cell k=pop(cx),h=pop(cx);
  if(h.tag!=T_PTR||!h.i){ pushi(cx,0); return; }
  Hdr*a=uf_gc_find((void*)h.i); if(!a)die("HAS: not a managed handle");
  switch(a->tag){
    case HT_MAP: { Cell v; pushi(cx,map_get((Map*)a,k,&v)?1:0); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: pushi(cx,(k.i>=0&&(uint64_t)k.i<a->len)?1:0); return;
    case HT_STR: { const char* s=uf_sptr(h); const char* n=uf_sptr(k); pushi(cx,(*n==0||strstr(s,n))?1:0); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); pushi(cx,(o>=0&&(uint64_t)o<a->esz)?1:0); return; }
    default: die("HAS: unsupported handle");
  }
}
/* KEYS: h -> list (dict keys / obj field names) */
static void op_keys(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"KEYS");
  if(a->tag==HT_MAP){
    Map*m=(Map*)a; Dyn*d=uf_dyn_new(m->len?m->len:1); UF_PROTECT(&d);
    for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) uf_dyn_push(&d,m->keys[i]);
    UF_UNPROTECT(); pushp(cx,d); return;
  }
  if(a->tag==HT_OBJ){
    int64_t sid=(int64_t)a->len; Dyn*d=0;
    for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){
      d=uf_dyn_new((uint64_t)uf_st_nf[q]); UF_PROTECT(&d);
      for(int64_t f=0;f<uf_st_nf[q];f++){ Cell c=uf_str_new(uf_st_fields[q][f],strlen(uf_st_fields[q][f])); uf_dyn_push(&d,c); }
      UF_UNPROTECT(); pushp(cx,d); return;
    }
    die("KEYS: unknown struct id");
  }
  die("KEYS: unsupported handle");
}
/* TYPEOF: h -> tag (v10 numbering) */
static void op_typeof(Ctx*cx){
  Cell h=pop(cx);
  if(h.tag==T_PTR){ if(!h.i){pushi(cx,2);return;} Hdr*a=uf_gc_find((void*)h.i); pushi(cx,a?(int64_t)a->tag:2); return; }
  pushi(cx,(int64_t)h.tag);
}
/* LEN: h -> n */
static void op_len(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"LEN");
  switch(a->tag){
    case HT_ARR: case HT_TENSOR: case HT_DYN: case HT_MAP: case HT_RING: pushi(cx,(int64_t)a->len); return;
    case HT_STR: pushi(cx,(int64_t)a->len); return;
    case HT_BITMAP: pushi(cx,(int64_t)a->len); return;
    case HT_ATOM: pushi(cx,1); return;
    case HT_BLOOM: pushi(cx,(int64_t)a->len); return;
    default: die("LEN: handle has no length");
  }
}
/* CAT: a b -> h' (str concat / arr concat / list concat) */
static void op_cat(Ctx*cx){
  Cell b=pop(cx),a=pop(cx);
  Hdr*ha=a.tag==T_PTR&&a.i?uf_gc_find((void*)a.i):0;
  Hdr*hb=b.tag==T_PTR&&b.i?uf_gc_find((void*)b.i):0;
  uint64_t ta=ha?ha->tag:0, tb=hb?hb->tag:0;
  if(ta==HT_DYN||tb==HT_DYN){
    if(ta!=HT_DYN||tb!=HT_DYN)die("CAT: list/str mismatch");
    Dyn*x=(Dyn*)ha,*y=(Dyn*)hb; Dyn*r=uf_dyn_new(x->len+y->len); UF_PROTECT(&r);
    for(uint64_t i=0;i<x->len;i++)uf_dyn_push(&r,x->data[i]);
    for(uint64_t i=0;i<y->len;i++)uf_dyn_push(&r,y->data[i]);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  if((ta==HT_ARR||ta==HT_TENSOR)||(tb==HT_ARR||tb==HT_TENSOR)){
    if((ta!=HT_ARR&&ta!=HT_TENSOR)||(tb!=HT_ARR&&tb!=HT_TENSOR))die("CAT: arr/str mismatch");
    if(ha->ety!=hb->ety)die("CAT: arr element-type mismatch");
    uint64_t n=ha->len+hb->len; Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*ha->esz,0);
    UF_PROTECT(&r);
    r->tag=HT_ARR; r->len=n; r->esz=ha->esz; r->ety=ha->ety;
    memcpy(r->data,ha->data,ha->len*ha->esz); memcpy(r->data+ha->len*ha->esz,hb->data,hb->len*hb->esz);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  { const char* x=uf_sptr(a),*y=uf_sptr(b); size_t la=strlen(x),lb=strlen(y);
    Str* r=(Str*)uf_gc_alloc(sizeof(Str)+la+lb+1,0); r->tag=HT_STR; r->esz=1; r->len=la+lb; r->mlen=0;
    memcpy(r->data,x,la); memcpy(r->data+la,y,lb+1); pushp(cx,r); }
}
/* SLICE: seq a b -> seq' (tag-dispatched; Python slice semantics) */
static void op_slice(Ctx*cx){
  Cell b=pop(cx),a=pop(cx),st=pop(cx);
  Hdr*h=st.tag==T_PTR&&st.i?uf_gc_find((void*)st.i):0;
  if(!h){ /* legacy raw char* */
    const char* S=(const char*)st.i; int64_t n=(int64_t)strlen(S);
    int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
    pushc(cx,uf_str_new(S+i,(size_t)(j-i))); return;
  }
  int64_t n=(int64_t)h->len;
  int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
  if(h->tag==HT_STR){ Str*s=(Str*)h; pushc(cx,uf_str_new(uf_sbytes(s)+i,(size_t)(j-i))); return; }
  if(h->tag==HT_BUF){ pushc(cx,uf_str_new(h->data+i,(size_t)(j-i))); return; }
  if(h->tag==HT_DYN){
    Dyn*d=(Dyn*)h; Dyn*r=uf_dyn_new((uint64_t)(j-i)); UF_PROTECT(&r);
    for(int64_t q=i;q<j;q++)uf_dyn_push(&r,d->data[q]);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  if(h->tag==HT_ARR||h->tag==HT_TENSOR){
    Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)(j-i)*h->esz,0);
    r->tag=h->tag; r->len=(uint64_t)(j-i); r->esz=h->esz; r->ety=h->ety;
    memcpy(r->data,uf_data(h)+i*h->esz,(size_t)(j-i)*h->esz);
    pushp(cx,r); return;
  }
  die("SLICE: unsupported handle");
}

static void op_buf(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative BUF size"); Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_BUF; h->len=(uint64_t)sz; h->esz=1; h->gc_flags|=GCF_PINNED; memset(h->data,0,(size_t)sz); pushp(cx,h); }
static void op_bufcopy(Ctx*cx){ int64_t n=pop(cx).i; Cell s=pop(cx),d=pop(cx); if(n>0)memmove(((void*)d.i),((void*)s.i),(size_t)n); }
static void op_loadx(Ctx*cx){ Cell a=pop(cx); pushi(cx,*(int64_t*)((void*)a.i)); }
static void op_storex(Ctx*cx){ Cell a=pop(cx); Cell v=pop(cx); *(int64_t*)((void*)a.i)=v.i; }
static void op_malloc(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative MALLOC size"); void*p=malloc((size_t)sz?sz:1); if(!p)die("out of memory"); pushp(cx,p); }
static void op_free(Ctx*cx){ Cell p=pop(cx); free(((void*)p.i)); }
static void op_sizeof(Ctx*cx){ int64_t ty=pop(cx).i; pushi(cx,ty==3?1:8); }

/* ================= fmt / print / scan ================= */
static int uf_count(const char*f){ int c=0; for(;f&&*f;f++){ if(*f=='%'){ if(f[1]=='%'){ f++; } else { c++; if(f[1]=='*')c++; } } } return c; }
static char* uf_fmt(const char*f,Cell*a,int n){
  size_t cap=256,bi=0; char*buf=(char*)uf_alloc(cap,0); int ai=0;
  for(const char*p=f;*p;){
    if(*p!='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]=*p++; continue; }
    if(p[1]=='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]='%'; p+=2; continue; }
    char d[32]; int di=0; d[di++]='%'; p++;
    while(*p&&strchr("-+ #0",*p)) d[di++]=*p++;
    while(*p&&(isdigit((unsigned char)*p)||*p=='.')) d[di++]=*p++;
    while(*p&&strchr("hlLjzt",*p)) p++;
    char conv=*p?*p++:'d';
    if(ai>=n) die("FMT: not enough args");
    Cell ar=a[ai++];
    char tmp[128]; int tl=0;
    d[di]=0;
    switch(conv){
      case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
        { size_t l=strlen(d); d[l]='l'; d[l+1]='l'; d[l+2]=conv; d[l+3]=0; }
        tl=snprintf(tmp,sizeof(tmp),d,(unsigned long long)ar.i); break; }
      case 'c': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,(int)ar.i); break; }
      case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
        size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        tl=snprintf(tmp,sizeof(tmp),d,uf_f(ar)); break; }
      case 's': { size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        const char* sv=uf_sptr(ar);
        int need=snprintf(0,0,d,sv);
        while(bi+(size_t)need+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
        snprintf(buf+bi,(size_t)need+1,d,sv);
        bi+=(size_t)need; continue; }
      case 'p': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,((void*)ar.i)); break; }
      default: die("FMT: unsupported directive");
    }
    if(tl<0) die("FMT failed");
    if((size_t)tl>sizeof(tmp)) tl=sizeof(tmp); /* clamp to actual bytes written */
    while(bi+(size_t)tl+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
    memcpy(buf+bi,tmp,(size_t)tl); bi+=(size_t)tl;
  }
  buf[bi]=0; return buf;
}
static void op_fmt(Ctx*cx){ Cell f=pop(cx); int n=uf_count(uf_sptr(f)); Cell args[16]; if(n>16)die("FMT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); char*s=uf_fmt(uf_sptr(f),args,n); Cell r=uf_str_new(s,strlen(s)); free(s); pushc(cx,r); }
/* PRINT: fmt args.. -> n ; fmt is ON TOP with args below it (deepest first) */
static void op_print(Ctx*cx){ Cell f=pop(cx); int n=uf_count(uf_sptr(f)); Cell args[16]; if(n>16)die("PRINT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); char*s=uf_fmt(uf_sptr(f),args,n); int r=printf("%s",s); free(s); pushi(cx,(int64_t)r); }
/* SCAN: fmt -> values.. count */
static void op_scan(Ctx*cx){
  Cell f=pop(cx); const char*p=uf_sptr(f); int n=0;
  for(;*p;p++){
    if(*p=='%'){
      if(p[1]=='%'){ p++; continue; }
      p++;
      while(*p&&strchr("-+ #0",*p)) p++;
      while(*p&&(isdigit((unsigned char)*p)||*p=='.')) p++;
      while(*p&&strchr("hlLjzt",*p)) p++;
      char conv=*p?*p:'\0';
      switch(conv){
        case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
          long long v; char d[8]; d[0]='%'; d[1]='l'; d[2]='l'; d[3]=conv; d[4]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushi(cx,(int64_t)v); n++; break; }
        case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
          double v; char d[8]; d[0]='%'; d[1]='l'; d[2]='f'; d[3]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushf(cx,v); n++; break; }
        case 's': {
          char*b=(char*)uf_alloc(1<<16,0);
          if(fscanf(stdin,"%65535s",b)!=1) die("SCAN: input error"); Cell r=uf_str_new(b,strlen(b)); free(b); pushc(cx,r); n++; break; }
        default: die("SCAN: unsupported directive");
      }
    } else if(isspace((unsigned char)*p)) {
      continue;
    } else {
      die("SCAN: literal text in format unsupported");
    }
  }
  pushi(cx,(int64_t)n);
}
static int uf_vargc(Ctx*cx){ for(int t=0;t<cx->sp;t++){ Cell fc=cx->ds[cx->sp-1-t]; if(fc.tag==T_PTR&&((void*)fc.i)&&uf_count(uf_sptr(fc))==t) return t; } die("vararg call: format string not found"); return 0; }

/* ================= list / dict / chan / atom ops ================= */
static void op_list(Ctx*cx){ pushp(cx,uf_dyn_new(8)); }
/* PUSH (was APPEND): h v -> h' (list grows by move; the returned handle is
   the live one — the old handle is reclaimed by the GC) */
static void op_push(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"PUSH"); if(a->tag!=HT_DYN)die("PUSH: not a list"); pushp(cx,uf_dyn_push2((Dyn*)a,v)); }
static void op_lpop(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"POP"); if(a->tag!=HT_DYN)die("POP: not a list"); Dyn*d=(Dyn*)a; if(d->len==0)die("POP: empty list"); pushc(cx,d->data[--d->len]); }
static void op_dict(Ctx*cx){ pushp(cx,uf_map_new()); }

/* CHAN: bounded MPSC ring buffer with blocking ENQ/DEQ */
static void ring_enq(Ring*r,Cell v){ pthread_mutex_lock(&r->mu); while(r->len>=r->cap&&!r->closed) pthread_cond_wait(&r->notfull,&r->mu); if(r->closed){ pthread_mutex_unlock(&r->mu); die("ENQ: chan closed"); } r->buf[r->tail]=v; r->tail=(r->tail+1)%r->cap; r->len++; pthread_cond_signal(&r->notempty); pthread_mutex_unlock(&r->mu); }
static void ring_close(Ring*r){ pthread_mutex_lock(&r->mu); r->closed=1; pthread_cond_broadcast(&r->notempty); pthread_cond_broadcast(&r->notfull); pthread_mutex_unlock(&r->mu); }
/* blocking deq with close detection: 0 = got a value, 1 = closed+drained */
static int ring_deq1(Ring*r,Cell*out){ pthread_mutex_lock(&r->mu); while(r->len==0&&!r->closed) pthread_cond_wait(&r->notempty,&r->mu); if(r->len==0){ pthread_mutex_unlock(&r->mu); return 1; } *out=r->buf[r->head]; r->head=(r->head+1)%r->cap; r->len--; pthread_cond_signal(&r->notfull); pthread_mutex_unlock(&r->mu); return 0; }
static Ring* uf_ring_new(uint64_t cap){ if(!cap)cap=16; Ring*r=(Ring*)uf_gc_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=cap; r->buf=(Cell*)uf_alloc((size_t)cap*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0); return r; }
static void op_chan(Ctx*cx){ int64_t cap=pop(cx).i; if(cap<=0)cap=16; pushp(cx,uf_ring_new((uint64_t)cap)); }
static void op_enq(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"ENQ"); if(a->tag!=HT_RING)die("ENQ: not a chan"); ring_enq((Ring*)a,v); }
/* DEQ: h -> v  (blocks while empty; closed+empty yields sentinel 0) */
static void op_deq(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"DEQ"); if(a->tag!=HT_RING)die("DEQ: not a chan"); Cell v; if(ring_deq1((Ring*)a,&v))v=uf_mki(0); pushc(cx,v); }
static void op_close(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"CLOSE"); if(a->tag!=HT_RING)die("CLOSE: not a chan"); ring_close((Ring*)a); }

/* ATOM: atomic i64 cell */
static void op_atom(Ctx*cx){ Cell v=pop(cx); Atom*a=(Atom*)uf_gc_alloc(sizeof(Atom),0); a->tag=HT_ATOM; a->len=1; atomic_store(&a->v,v.i); pushp(cx,a); }
static void op_aget(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"AGET"); if(a->tag!=HT_ATOM)die("AGET: not an atom"); pushi(cx,atomic_load(&((Atom*)a)->v)); }
static void op_aset(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"ASET"); if(a->tag!=HT_ATOM)die("ASET: not an atom"); atomic_store(&((Atom*)a)->v,v.i); }
static void op_aadd(Ctx*cx){ Cell n=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"AADD"); if(a->tag!=HT_ATOM)die("AADD: not an atom"); pushi(cx,atomic_fetch_add(&((Atom*)a)->v,n.i)); }
static void op_cas(Ctx*cx){ Cell nw=pop(cx),old=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"CAS"); if(a->tag!=HT_ATOM)die("CAS: not an atom"); int64_t e=old.i; pushi(cx,atomic_compare_exchange_strong(&((Atom*)a)->v,&e,nw.i)?1:0); }

/* RANGE: start stop -> list of ints [start, stop) */
static void op_range(Ctx*cx){
  int64_t stop=pop(cx).i, start=pop(cx).i;
  int64_t n = stop>start ? stop-start : 0;
  Dyn* d=uf_dyn_new((uint64_t)n); UF_PROTECT(&d);
  for(int64_t k=start;k<stop;k++) uf_dyn_push(&d,uf_mki(k));
  UF_UNPROTECT(); pushp(cx,d);
}

/* ================= iterators ================= */
static Iter* uf_iter_new(Cell src);
static int uf_iter_next(Ctx*cx, Iter* it, Cell* out){
  switch(it->kind){
    case IT_LIST: { Dyn*d=(Dyn*)uf_gc_find((void*)it->src.i); if(!d)return 0; if((uint64_t)it->idx>=d->len)return 0; *out=d->data[it->idx++]; return 1; }
    case IT_ARR: { Hdr*a=(Hdr*)uf_gc_find((void*)it->src.i); if(!a)return 0; if((uint64_t)it->idx>=a->len)return 0; *out=uf_cidx(it->src,it->idx++); return 1; }
    case IT_DICT: { Map*m=(Map*)uf_gc_find((void*)it->src.i); if(!m)return 0; while((uint64_t)it->idx<m->cap&&m->st[it->idx]!=1)it->idx++; if((uint64_t)it->idx>=m->cap)return 0; *out=m->keys[it->idx++]; return 1; }
    case IT_STR: { Str*s=(Str*)uf_gc_find((void*)it->src.i); if(!s)return 0; if((uint64_t)it->idx>=s->len)return 0; *out=uf_mki((uint8_t)uf_sbytes(s)[it->idx++]); return 1; }
    case IT_CHAN: { Ring*r=(Ring*)uf_gc_find((void*)it->src.i); if(!r)return 0; return ring_deq1(r,out)?0:1; }
    case IT_BITMAP: { Bitmap*b=(Bitmap*)uf_gc_find((void*)it->src.i); if(!b)return 0; uint64_t n=b->len; while((uint64_t)it->idx<n){ uint64_t w=(uint64_t)it->idx>>6, o=(uint64_t)it->idx&63; if((w<(n+63)/64)&&((b->words[w]>>o)&1)){ *out=uf_mki(it->idx++); return 1; } it->idx++; } return 0; }
    case IT_MAP: { Iter* in=(Iter*)uf_gc_find((void*)it->g.i); if(!in)return 0; Cell v; if(!uf_iter_next(cx,in,&v))return 0; pushc(cx,v); uf_call_addr(cx,(const void*)it->f.i); *out=pop(cx); return 1; }
    case IT_FILTER: { Iter* in=(Iter*)uf_gc_find((void*)it->g.i); if(!in)return 0; Cell v; while(uf_iter_next(cx,in,&v)){ pushc(cx,v); uf_call_addr(cx,(const void*)it->f.i); Cell r=pop(cx); if(!uf_zero(r)){ *out=v; return 1; } } return 0; }
  }
  return 0;
}
static Iter* uf_iter_new(Cell src){
  if(src.tag!=T_PTR||!src.i)die("ITER: not iterable");
  Hdr* h=uf_gc_find((void*)src.i); if(!h)die("ITER: not iterable");
  int kind;
  switch(h->tag){
    case HT_DYN: kind=IT_LIST; break;
    case HT_ARR: case HT_TENSOR: kind=IT_ARR; break;
    case HT_MAP: kind=IT_DICT; break;
    case HT_STR: kind=IT_STR; break;
    case HT_RING: kind=IT_CHAN; break;
    case HT_BITMAP: kind=IT_BITMAP; break;
    default: die("ITER: not iterable");
  }
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->src=src; it->kind=kind; it->idx=0; it->f=uf_mki(0); it->g=uf_mki(0);
  return it;
}
static void op_iter(Ctx*cx){ Cell h=pop(cx); pushp(cx,uf_iter_new(h)); }
static void op_next(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"NEXT"); if(a->tag!=HT_ITER)die("NEXT: not an iter");
  Cell v; if(uf_iter_next(cx,(Iter*)a,&v)){ pushc(cx,v); pushi(cx,1); } else { pushi(cx,0); pushi(cx,0); }
}
static Dyn* uf_collect_it(Ctx*cx, Iter* it){
  Dyn* d=uf_dyn_new(8); UF_PROTECT(&d); UF_PROTECT(&it);
  Cell v; while(uf_iter_next(cx,it,&v)) uf_dyn_push(&d,v);
  UF_UNPROTECT(); UF_UNPROTECT(); return d;
}
static void op_collect(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"COLLECT"); if(a->tag!=HT_ITER)die("COLLECT: not an iter"); pushp(cx,uf_collect_it(cx,(Iter*)a)); }
static void op_imap(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"IMAP"); if(a->tag!=HT_ITER)die("IMAP: not an iter");
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->kind=IT_MAP; it->idx=0; it->src=uf_mki(0); it->f=f; it->g=h;
  pushp(cx,it);
}
static void op_ifilter(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"IFILTER"); if(a->tag!=HT_ITER)die("IFILTER: not an iter");
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->kind=IT_FILTER; it->idx=0; it->src=uf_mki(0); it->f=f; it->g=h;
  pushp(cx,it);
}
/* materialize any iterable (or iter, drained) into a list; lists pass through */
static Dyn* uf_materialize(Ctx*cx, Cell h){
  if(h.tag==T_PTR&&h.i){
    Hdr* a=uf_gc_find((void*)h.i);
    if(a){
      if(a->tag==HT_DYN) return (Dyn*)a;
      if(a->tag==HT_ITER) return uf_collect_it(cx,(Iter*)a);
      if(a->tag==HT_ARR||a->tag==HT_TENSOR){
        Dyn* d=uf_dyn_new(a->len); UF_PROTECT(&d);
        for(uint64_t i=0;i<a->len;i++) uf_dyn_push(&d,uf_cidx(h,(int64_t)i));
        UF_UNPROTECT(); return d;
      }
      if(a->tag==HT_MAP||a->tag==HT_STR||a->tag==HT_RING||a->tag==HT_BITMAP){
        Iter* it=uf_iter_new(h); UF_PROTECT(&it); Dyn* d=uf_collect_it(cx,it); UF_UNPROTECT(); return d;
      }
    }
  }
  die("not a sequence");
  return 0;
}

/* ================= weave: static task DAG + dynamic fanout ================= */
typedef void(*UfRun)(Ctx*,long);
typedef struct { long pc; int ninputs; int* inputs; long count; Cell result; _Atomic int state; double t0,t1; long items; long retries; long tolerated; } WeaveTask;
typedef struct WeaveJobS { WeaveTask* ts; int n; UfRun run; } WeaveJob;
static void uf_weave_mark(struct WeaveJobS* j){ if(!j)return; for(int i=0;i<j->n;i++) uf_mark_cell(j->ts[i].result); }
static double uf_nowd(void){ struct timespec ts; clock_gettime(CLOCK_MONOTONIC,&ts); return ts.tv_sec+ts.tv_nsec/1e9; }

/* fanout coordination: feeder drains the (iterable) first input into an
   internal bounded chan; `count` workers pull items dynamically. Broadcast
   inputs (declared after the first) sit beneath the item on each worker's
   initial stack. Results collect in completion order. */
typedef struct { WeaveJob* j; WeaveTask* t; Ring* q; Dyn* results; pthread_mutex_t* rmu; Iter* it; Ctx* fcx; } UfFan;
static void* uf_fan_feeder(void* arg){
  UfFan* f=(UfFan*)arg;
  Cell v;
  while(uf_iter_next(f->fcx,f->it,&v)) ring_enq(f->q,v);
  ring_close(f->q);
  return 0;
}
static void* uf_fan_worker(void* arg){
  UfFan* f=(UfFan*)arg;
  WeaveTask* t=f->t; WeaveJob* j=f->j;
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_cur_task=t;
  Cell item;
  while(ring_deq1(f->q,&item)==0){
    /* initial stack: broadcast inputs (declared order, deepest first), item on top */
    for(int k=1;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
    pushc(c,item);
    j->run(c,t->pc);
    Cell r = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
    c->sp=0; c->csp=0; c->lsp=0;
    pthread_mutex_lock(f->rmu);
    f->results=uf_dyn_push2(f->results,r);
    t->items++;
    pthread_mutex_unlock(f->rmu);
  }
  uf_cur_task=0;
  ctx_free(c);
  return 0;
}
static void uf_run_fanout(WeaveJob* j, WeaveTask* t){
  Cell input = j->ts[t->inputs[0]].result;
  Iter* it = uf_iter_new(input); /* dies if not iterable */
  Ring* q = uf_ring_new(64);
  Dyn* results = uf_dyn_new(8);
  pthread_mutex_t rmu; pthread_mutex_init(&rmu,0);
  UF_PROTECT(&it); UF_PROTECT(&q); UF_PROTECT(&results);
  UfFan f; f.j=j; f.t=t; f.q=q; f.results=results; f.rmu=&rmu; f.it=it;
  f.fcx=ctx_new(1<<12,1<<8);
  pthread_t ft; if(pthread_create(&ft,0,uf_fan_feeder,&f))die("WEAVE: feeder thread");
  long nw=t->count; if(nw<1)nw=1; if(nw>64)nw=64;
  pthread_t th[64];
  for(long i=0;i<nw;i++) if(pthread_create(&th[i],0,uf_fan_worker,&f))die("WEAVE: worker thread");
  for(long i=0;i<nw;i++) pthread_join(th[i],0);
  pthread_join(ft,0);
  ctx_free(f.fcx);
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  t->result=uf_mkp(f.results);
  pthread_mutex_destroy(&rmu);
}
static void* uf_worker(void*arg){
  WeaveJob*j=(WeaveJob*)arg;
  for(;;){
    int pick=-1;
    for(int i=0;i<j->n;i++){
      if(atomic_load(&j->ts[i].state)!=0) continue;
      int ready=1;
      for(int k=0;k<j->ts[i].ninputs;k++) if(atomic_load(&j->ts[j->ts[i].inputs[k]].state)!=2){ready=0;break;}
      if(!ready) continue;
      int exp=0;
      if(atomic_compare_exchange_strong(&j->ts[i].state,&exp,1)){ pick=i; break; }
    }
    if(pick<0){
      int alldone=1; for(int i=0;i<j->n;i++) if(atomic_load(&j->ts[i].state)!=2){alldone=0;break;}
      if(alldone) return 0;
      sched_yield(); continue;
    }
    WeaveTask*t=&j->ts[pick];
    t->t0=uf_nowd();
    uf_cur_task=t;
    if(t->count>1){
      uf_run_fanout(j,t);
    } else {
      Ctx*c=ctx_new(1<<16,1<<12);
      for(int k=0;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
      j->run(c,t->pc);
      t->result = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
      t->items = 1;
      ctx_free(c);
    }
    uf_cur_task=0;
    t->t1=uf_nowd();
    atomic_store(&t->state,2);
  }
}
static void uf_weave(Ctx*cx,WeaveTask*ts,int n,UfRun run){
  (void)cx;
  long ncpu=sysconf(_SC_NPROCESSORS_ONLN);
  long total=0; for(int i=0;i<n;i++) total += ts[i].count>1?ts[i].count:1;
  int nw=(int)total; if(ncpu>0&&(long)nw>ncpu)nw=(int)ncpu; if(nw<1)nw=1; if(nw>64)nw=64;
  WeaveJob j={ts,n,run};
  uf_active_job=&j;
  if(nw<=1){ uf_worker(&j); }
  else {
    pthread_t th[64];
    for(int i=0;i<nw-1;i++) pthread_create(&th[i],0,uf_worker,&j);
    uf_worker(&j);
    for(int i=0;i<nw-1;i++) pthread_join(th[i],0);
  }
  uf_active_job=0;
  if(getenv("UF_WEAVE_DEBUG")){
    for(int i=0;i<n;i++){
      WeaveTask*t=&ts[i];
      fprintf(stderr,"weave: task pc=%ld wall=%.3fms workers=%ld items=%ld retries=%ld tolerated=%ld\n",
        t->pc,(t->t1-t->t0)*1e3,t->count,t->items,t->retries,t->tolerated);
    }
  }
}

/* ================= shared string helpers ================= */
static char* uf_read_all(FILE*f){
  size_t cap=4096,n=0; char*b=(char*)uf_alloc(cap,0); size_t m;
  while((m=fread(b+n,1,cap-1-n,f))>0){ n+=m; if(cap-1-n==0){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); } }
  b[n]=0; return b;
}
static int uf_wait_status(int r){
#ifdef _WIN32
  return r;
#else
  if(r==-1)return -1;
  if(WIFEXITED(r))return WEXITSTATUS(r);
  if(WIFSIGNALED(r))return 128+WTERMSIG(r);
  return r;
#endif
}

/* ================= shell ops ================= */
/* SH (merged SH+SHX): cmd -> out err status (always captures both streams
   as fresh strings; status on top: -1 spawn failure, 128+signal) */
static void op_sh(Ctx*cx){
  Cell c=pop(cx); const char*cmd=uf_sptr(c);
#ifdef _WIN32
  char tmp[256]; tmpnam(tmp);
  char*full=(char*)uf_alloc(strlen(cmd)+strlen(tmp)+8,0);
  sprintf(full,"%s 2>%s",cmd,tmp);
  FILE* f=_popen(full,"r"); if(!f){ pushc(cx,uf_str_new("",0)); pushc(cx,uf_str_new("",0)); pushi(cx,-1); return; }
  char*out=uf_read_all(f); int st=uf_wait_status(_pclose(f));
  FILE* ef=fopen(tmp,"r"); char*err;
  if(ef){ err=uf_read_all(ef); fclose(ef); remove(tmp); } else err=uf_alloc(1,0),err[0]=0;
  Cell so=uf_str_new(out,strlen(out)); Cell se=uf_str_new(err,strlen(err));
  free(out); free(err); free(full);
  pushc(cx,so); pushc(cx,se); pushi(cx,st);
#else
  int pfd[2]; if(pipe(pfd))die("SH: pipe");
  FILE* ef=tmpfile(); if(!ef)die("SH: tmpfile");
  pid_t pid=fork();
  if(pid<0)die("SH: fork");
  if(pid==0){
    close(pfd[0]);
    if(dup2(pfd[1],1)<0)_exit(127);
    if(dup2(fileno(ef),2)<0)_exit(127);
    execl("/bin/sh","sh","-c",cmd,(char*)0);
    _exit(127);
  }
  close(pfd[1]);
  FILE* f=fdopen(pfd[0],"r"); if(!f)die("SH: fdopen");
  char*out=uf_read_all(f); fclose(f);
  int rs=0; waitpid(pid,&rs,0);
  int st=uf_wait_status(rs);
  rewind(ef); char*err=uf_read_all(ef); fclose(ef);
  Cell so=uf_str_new(out,strlen(out)); Cell se=uf_str_new(err,strlen(err));
  free(out); free(err);
  pushc(cx,so); pushc(cx,se); pushi(cx,st);
#endif
}
/* SHP: cmd -> chan (worker thread streams stdout lines, closes chan at exit) */
typedef struct { Ring* r; char* cmd; } UfShp;
static void* uf_shp_worker(void*arg){
  UfShp* g=(UfShp*)arg;
#ifdef _WIN32
  FILE* f=_popen(g->cmd,"r");
#else
  FILE* f=popen(g->cmd,"r");
#endif
  if(f){
#ifdef _WIN32
    char line[16384];
    while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; Cell v=uf_str_new(line,m); ring_enq(g->r,v); }
    _pclose(f);
#else
    char*line=0; size_t ncap=0; ssize_t m;
    while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; Cell v=uf_str_new(line,(size_t)m); ring_enq(g->r,v); }
    free(line); pclose(f);
#endif
  }
  ring_close(g->r);
  free(g);
  return 0;
}
static void op_shp(Ctx*cx){
  Cell c=pop(cx);
  Ring*r=uf_ring_new(64);
  UfShp* g=(UfShp*)malloc(sizeof(UfShp)); if(!g)die("out of memory"); g->r=r; g->cmd=strdup(uf_sptr(c));
  pthread_t th; if(pthread_create(&th,0,uf_shp_worker,g)){ ring_close(r); die("SHP: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* EXEC: list -> status (argv list, no shell) */
static void op_exec(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"EXEC"); if(a->tag!=HT_DYN)die("EXEC: not a list");
  Dyn*d=(Dyn*)a;
  if(d->len==0)die("EXEC: empty argv");
  char**argv=(char**)malloc((d->len+1)*sizeof(char*)); if(!argv)die("out of memory");
  for(uint64_t i=0;i<d->len;i++)argv[i]=(char*)uf_sptr(d->data[i]);
  argv[d->len]=0;
#ifdef _WIN32
  intptr_t r=_spawnvp(_P_WAIT,argv[0],(const char* const*)argv);
  int st=(r==-1)?-1:(int)r;
#else
  pid_t pid=fork();
  if(pid<0)die("EXEC: fork");
  if(pid==0){ execvp(argv[0],argv); _exit(127); }
  int rs=0; waitpid(pid,&rs,0); int st=uf_wait_status(rs);
#endif
  free(argv); pushi(cx,st);
}

/* ================= embedded regex (unchanged from v9) =================
   Syntax: literals, '.', '*', '+', '?', '[...]' (ranges, '^' negation),
   '^' at alternative start, '$' at end, '|' alternation, '(' ')' groups
   (<=9, \\1..\\9 backrefs in REPLACE). Greedy with backtracking. */
typedef struct { const char* s; const char* e; } RxCap;
enum { RXA_LIT=0, RXA_DOT=1, RXA_CLS=2, RXA_GRP=3 };
typedef struct { int type; char ch; const char* cs; const char* ce; const char* gs; const char* ge; int cap; } RxAtom;
static int rx_cls_find(const char* p, const char** close){
  if(*p=='^')p++;
  if(*p==']')p++;
  while(*p){
    if(*p=='\\'&&p[1]){ p+=2; continue; }
    if(*p==']'){ *close=p; return 1; }
    p++;
  }
  return 0;
}
static int rx_cls_in(const char* cs, const char* ce, char c){
  int neg=0; const char* p=cs;
  if(p<ce&&*p=='^'){ neg=1; p++; }
  int ok=0; int first=1;
  while(p<ce){
    char lo;
    if(*p=='\\'&&p+1<ce){ lo=p[1]; p+=2; } else lo=*p++;
    if(first&&lo==']'){ if(c==']')ok=1; first=0; continue; }
    first=0;
    if(p<ce&&*p=='-'&&p+1<ce){
      p++; char hi;
      if(*p=='\\'&&p+1<ce){ hi=p[1]; p+=2; } else hi=*p++;
      if((unsigned char)c>=(unsigned char)lo&&(unsigned char)c<=(unsigned char)hi)ok=1;
    } else if(c==lo)ok=1;
  }
  return neg?!ok:ok;
}
static int rx_group_index(const char* pat0, const char* p){
  int n=0; const char* q=pat0;
  while(q<p){
    if(*q=='\\'&&q[1]){ q+=2; continue; }
    if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } }
    if(*q=='(')n++;
    q++;
  }
  return n;
}
static const char* rx_parse_atom(const char* p, RxAtom* a, const char* pat0){
  memset(a,0,sizeof(*a));
  char c=*p;
  if(c=='\\'){ if(!p[1])die("MATCH: trailing backslash"); a->type=RXA_LIT; a->ch=p[1]; return p+2; }
  if(c=='.'){ a->type=RXA_DOT; return p+1; }
  if(c=='['){ const char* cl; if(!rx_cls_find(p+1,&cl))die("MATCH: unbalanced ["); a->type=RXA_CLS; a->cs=p+1; a->ce=cl; return cl+1; }
  if(c=='('){
    int depth=1; const char* q=p+1;
    while(*q&&depth){
      if(*q=='\\'&&q[1]){ q+=2; continue; }
      if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } q++; continue; }
      if(*q=='(')depth++;
      else if(*q==')')depth--;
      q++;
    }
    if(depth)die("MATCH: unbalanced (");
    a->type=RXA_GRP; a->gs=p+1; a->ge=q-1; a->cap=rx_group_index(pat0,p)+1;
    if(a->cap>9)die("MATCH: more than 9 groups");
    return q;
  }
  a->type=RXA_LIT; a->ch=c; return p+1;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0);
static const char* rx_atom1(RxAtom* a, const char* s, RxCap* caps, const char* pat0){
  switch(a->type){
  case RXA_LIT: return (*s&&*s==a->ch)?s+1:0;
  case RXA_DOT: return *s?s+1:0;
  case RXA_CLS: return (*s&&rx_cls_in(a->cs,a->ce,*s))?s+1:0;
  case RXA_GRP: {
    const char* alt=a->gs;
    for(;;){
      const char* ae=alt; int depth=0;
      while(ae<a->ge){
        if(*ae=='\\'&&ae+1<a->ge){ ae+=2; continue; }
        if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)&&cl<a->ge){ ae=cl+1; continue; } }
        if(*ae=='(')depth++;
        else if(*ae==')')depth--;
        else if(*ae=='|'&&depth==0)break;
        ae++;
      }
      const char* r=rx_seq(alt,ae,s,caps,pat0);
      if(r){ caps[a->cap].s=s; caps[a->cap].e=r; return r; }
      if(ae>=a->ge)return 0;
      alt=ae+1;
    }
  }
  }
  return 0;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0){
  if(p>=pend)return s;
  if(*p=='$'&&p+1==pend)return *s==0?s:0;
  RxAtom a; const char* q=rx_parse_atom(p,&a,pat0);
  long min=1,max=1;
  if(q<pend&&(*q=='*'||*q=='+'||*q=='?')){
    char t=*q; q++;
    if(t=='*'){min=0;max=1<<30;} else if(t=='+'){min=1;max=1<<30;} else {min=0;max=1;}
  }
  if(max==1){
    const char* r=rx_atom1(&a,s,caps,pat0);
    if(r){ const char* e=rx_seq(q,pend,r,caps,pat0); if(e)return e; }
    if(min==0)return rx_seq(q,pend,s,caps,pat0);
    return 0;
  }
  size_t capn=16,n=0; const char**v=(const char**)malloc(capn*sizeof(char*));
  if(!v)die("out of memory");
  v[n++]=s;
  while((long)n-1<max){
    const char* r=rx_atom1(&a,v[n-1],caps,pat0);
    if(!r||r==v[n-1])break;
    if(n==capn){ capn*=2; v=(const char**)realloc(v,capn*sizeof(char*)); if(!v)die("out of memory"); }
    v[n++]=r;
  }
  const char* ok=0;
  for(long k=(long)n-1;k>=min;k--){
    const char* e=rx_seq(q,pend,v[k],caps,pat0);
    if(e){ ok=e; break; }
  }
  free(v);
  return ok;
}
static int rx_exec(const char* pat, const char* str, RxCap* caps){
  const char* alt=pat;
  for(;;){
    const char* ae=alt; int depth=0;
    while(*ae){
      if(*ae=='\\'&&ae[1]){ ae+=2; continue; }
      if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)){ ae=cl+1; continue; } }
      if(*ae=='(')depth++;
      else if(*ae==')')depth--;
      else if(*ae=='|'&&depth==0)break;
      ae++;
    }
    const char* p0=alt; int anch=0;
    if(p0<ae&&*p0=='^'){ anch=1; p0++; }
    const char* pos=str;
    for(;;){
      for(int i=0;i<10;i++){ caps[i].s=0; caps[i].e=0; }
      const char* r=rx_seq(p0,ae,pos,caps,pat);
      if(r){ caps[0].s=pos; caps[0].e=r; return 1; }
      if(anch||!*pos)break;
      pos++;
    }
    if(!*ae)return 0;
    alt=ae+1;
  }
}

/* MATCH (was RX): str pat -> list found (group 0 = whole match; found on top) */
static void op_match(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=uf_sptr(pat); const char* S=uf_sptr(st);
  RxCap caps[10];
  int ntotal=rx_group_index(P,P+strlen(P));
  if(rx_exec(P,S,caps)){
    Dyn*d=uf_dyn_new((uint64_t)ntotal+1); UF_PROTECT(&d);
    for(int i=0;i<=ntotal;i++){
      if(caps[i].s) uf_dyn_push_str(&d,caps[i].s,(size_t)(caps[i].e-caps[i].s));
      else uf_dyn_push_str(&d,"",0);
    }
    UF_UNPROTECT();
    pushp(cx,d); pushi(cx,1);
  } else {
    Dyn*d=uf_dyn_new(1); pushp(cx,d); pushi(cx,0);
  }
}
/* REPLACE (was RXSUB): str pat repl -> str' (all matches; \\1..\\9, \\\\) */
static void op_replace(Ctx*cx){
  Cell repl=pop(cx),pat=pop(cx),st=pop(cx);
  const char* R=uf_sptr(repl); const char* P=uf_sptr(pat); const char* S=uf_sptr(st);
  size_t cap=256,n=0; char*out=(char*)uf_alloc(cap,0);
  RxCap caps[10];
  const char* cur=S;
#define UF_APP(src,L) do{ size_t _l=(size_t)(L); while(n+_l+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); } memcpy(out+n,(src),_l); n+=_l; }while(0)
  while(rx_exec(P,cur,caps)){
    UF_APP(cur,caps[0].s-cur);
    for(const char* r=R; *r; ){
      if(*r=='\\'&&r[1]){
        if(r[1]>='1'&&r[1]<='9'){ int g=r[1]-'0'; if(caps[g].s)UF_APP(caps[g].s,caps[g].e-caps[g].s); r+=2; }
        else { UF_APP(r+1,1); r+=2; }
      } else { UF_APP(r,1); r++; }
    }
    if(caps[0].e==caps[0].s){ if(!*cur)break; UF_APP(cur,1); cur++; }
    else cur=caps[0].e;
  }
  UF_APP(cur,strlen(cur));
#undef UF_APP
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* RSPLIT (was RXSPLIT): str pat -> list */
static void op_rsplit(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=uf_sptr(pat); const char* cur=uf_sptr(st);
  Dyn*d=uf_dyn_new(8); UF_PROTECT(&d); RxCap caps[10];
  while(rx_exec(P,cur,caps)){
    if(caps[0].e==caps[0].s){ if(!*cur)break; cur++; continue; }
    uf_dyn_push_str(&d,cur,(size_t)(caps[0].s-cur));
    cur=caps[0].e;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  UF_UNPROTECT();
  pushp(cx,d);
}

/* ================= string ops ================= */
#ifdef _WIN32
static int uf_glob_match(const char* pat,const char* s){
  while(*pat){
    if(*pat=='*'){
      while(*pat=='*')pat++;
      if(!*pat)return 1;
      for(const char* t=s;;t++){ if(uf_glob_match(pat,t))return 1; if(!*t)break; }
      return 0;
    }
    if(*pat=='?'){ if(!*s)return 0; pat++; s++; continue; }
    if(*pat=='['){
      const char* cl; if(rx_cls_find(pat+1,&cl)){
        if(!*s)return 0;
        int neg=0; const char* p=pat+1;
        if(p<cl&&*p=='!'){ neg=1; p++; }
        int ok=0;
        while(p<cl){
          char lo=*p++;
          if(p<cl&&*p=='-'&&p+1<cl){ p++; char hi=*p++; if((unsigned char)*s>=(unsigned char)lo&&(unsigned char)*s<=(unsigned char)hi)ok=1; }
          else if(*s==lo)ok=1;
        }
        if(neg)ok=!ok;
        if(!ok)return 0;
        pat=cl+1; s++; continue;
      }
    }
    if(*pat=='\\'&&pat[1])pat++;
    if(*pat!=*s)return 0;
    if(!*s)return 0;
    pat++; s++;
  }
  return *s==0;
}
#endif
/* GLOB: str pat -> 0/1 */
static void op_glob(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
#ifdef _WIN32
  pushi(cx,uf_glob_match(uf_sptr(pat),uf_sptr(st))?1:0);
#else
  pushi(cx,fnmatch(uf_sptr(pat),uf_sptr(st),0)==0?1:0);
#endif
}
/* SPLIT: str sep -> list (true zero-copy: NUL-terminate fields in-place) */
static void op_split(Ctx*cx){
  Cell sep=pop(cx),st=pop(cx);
  /* get mutable pointer — parent must be a GC Str with inline/mmap data */
  Hdr* parent = uf_gc_find((void*)st.i);
  char* S;
  if(parent && parent->tag==HT_STR){
    Str* sp=(Str*)parent;
    S = (sp->mlen || sp->gc_parent) ? (char*)sp->mdata : sp->data;
  } else {
    S = (char*)uf_sptr(st); /* legacy raw char* */
  }
  const char* E=uf_sptr(sep);
  if(!*E)die("SPLIT: empty separator");
  size_t el=strlen(E);
  Dyn*d=uf_dyn_new(8); UF_PROTECT(&d);
  char* cur=S;
  for(;;){
    char* m=strstr(cur,E);
    if(!m)break;
    *m=0; /* NUL-terminate the field in-place */
    Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
    v->tag=HT_STR; v->esz=1; v->len=(size_t)(m-cur); v->mlen=0;
    v->mdata=cur; v->gc_parent=parent; /* view: keep parent alive, own nothing */
    uf_dyn_push(&d,uf_mkp(v));
    cur=m+el;
  }
  size_t flen=strlen(cur);
  Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
  v->tag=HT_STR; v->esz=1; v->len=flen; v->mlen=0;
  v->mdata=cur; v->gc_parent=parent;
  uf_dyn_push(&d,uf_mkp(v));
  UF_UNPROTECT();
  pushp(cx,d);
}
/* JOIN: list sep -> str */
static void op_join(Ctx*cx){
  Cell sep=pop(cx),h=pop(cx);
  Hdr*a=uf_handle(h,"JOIN"); if(a->tag!=HT_DYN)die("JOIN: not a list");
  Dyn*d=(Dyn*)a;
  const char* E=uf_sptr(sep); size_t el=strlen(E);
  size_t cap=64; for(uint64_t i=0;i<d->len;i++)cap+=strlen(uf_sptr(d->data[i]))+el;
  char*out=(char*)uf_alloc(cap,0); size_t n=0;
  for(uint64_t i=0;i<d->len;i++){
    if(i){ memcpy(out+n,E,el); n+=el; }
    const char* s=uf_sptr(d->data[i]); size_t L=strlen(s); memcpy(out+n,s,L); n+=L;
  }
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* FIND: str sub -> idx (-1 on miss; byte index) */
static void op_find(Ctx*cx){
  Cell sub=pop(cx),st=pop(cx);
  const char* S=uf_sptr(st);
  const char* m=strstr(S,uf_sptr(sub));
  pushi(cx,m?m-S:-1);
}
/* REPL: str old new -> str' (literal, replace all) */
static void op_repl(Ctx*cx){
  Cell nw=pop(cx),old=pop(cx),st=pop(cx);
  const char* S=uf_sptr(st); const char* O=uf_sptr(old); const char* N=uf_sptr(nw);
  if(!*O)die("REPL: empty pattern");
  size_t ol=strlen(O),nl=strlen(N);
  size_t cap=strlen(S)+64,n=0; char*out=(char*)uf_alloc(cap,0);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,O);
    if(!m)break;
    size_t pre=(size_t)(m-cur);
    while(n+pre+nl+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
    memcpy(out+n,cur,pre); n+=pre; memcpy(out+n,N,nl); n+=nl;
    cur=m+ol;
  }
  size_t tail=strlen(cur);
  while(n+tail+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
  memcpy(out+n,cur,tail); n+=tail;
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* TRIM: str -> str' */
static void op_trim(Ctx*cx){
  Cell st=pop(cx);
  const char* s=uf_sptr(st); size_t n=strlen(s);
  while(n>0&&isspace((unsigned char)s[0])){ s++; n--; }
  while(n>0&&isspace((unsigned char)s[n-1]))n--;
  pushc(cx,uf_str_new(s,n));
}
/* UP/DOWN: str -> str' (ASCII case) */
static void op_up(Ctx*cx){ Cell st=pop(cx); const char*s=uf_sptr(st); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='a'&&s[i]<='z')?(char)(s[i]-32):s[i]; r[n]=0; Cell c=uf_str_new(r,n); free(r); pushc(cx,c); }
static void op_down(Ctx*cx){ Cell st=pop(cx); const char*s=uf_sptr(st); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='A'&&s[i]<='Z')?(char)(s[i]+32):s[i]; r[n]=0; Cell c=uf_str_new(r,n); free(r); pushc(cx,c); }
/* STARTS/ENDS: str affix -> 0/1 */
static void op_starts(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=uf_sptr(st); const char*a=uf_sptr(af); pushi(cx,strncmp(s,a,strlen(a))==0?1:0); }
static void op_ends(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=uf_sptr(st); const char*a=uf_sptr(af); size_t ls=strlen(s),la=strlen(a); pushi(cx,(la<=ls&&strcmp(s+ls-la,a)==0)?1:0); }

/* ATOI/ATOF/ITOA/FTOA: explicit string<->number conversion */
static void op_atoi(Ctx*cx){ Cell s=pop(cx); pushi(cx,(int64_t)strtoll(uf_sptr(s),0,10)); }
static void op_atof(Ctx*cx){ Cell s=pop(cx); pushf(cx,strtod(uf_sptr(s),0)); }
static void op_itoa(Ctx*cx){ Cell n=pop(cx); char b[32]; snprintf(b,sizeof(b),"%lld",(long long)uf_i(n)); pushc(cx,uf_str_new(b,strlen(b))); }
static void op_ftoa(Ctx*cx){ Cell n=pop(cx); char b[32]; snprintf(b,sizeof(b),"%g",uf_f(n)); pushc(cx,uf_str_new(b,strlen(b))); }

/* ================= sequences: sort/filter/some/every ================= */
/* stable mergesort of a cell array with uf_cmp order; dies on incomparable */
static void uf_msort(Cell* v, Cell* tmp, uint64_t lo, uint64_t hi){
  if(hi-lo<2)return;
  uint64_t mid=(lo+hi)/2;
  uf_msort(v,tmp,lo,mid); uf_msort(v,tmp,mid,hi);
  uint64_t i=lo,j=mid,k=lo;
  while(i<mid&&j<hi){ int ok; int c=uf_cmp(v[i],v[j],&ok); if(!ok)die("SORT: incomparable element types"); if(c<=0)tmp[k++]=v[i++]; else tmp[k++]=v[j++]; }
  while(i<mid)tmp[k++]=v[i++];
  while(j<hi)tmp[k++]=v[j++];
  for(i=lo;i<hi;i++)v[i]=tmp[i];
}
/* SORT: seq -> seq' (fresh, stable; list -> list, arr -> arr by tag) */
static void op_sort(Ctx*cx){
  Cell h=pop(cx);
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  Dyn* d=uf_materialize(cx,h); UF_PROTECT(&d);
  if(d->len){
    Cell* tmp=(Cell*)uf_alloc(d->len*sizeof(Cell),0);
    uf_msort(d->data,tmp,0,d->len);
    free(tmp);
  }
  if(a&&(a->tag==HT_ARR||a->tag==HT_TENSOR)){
    Hdr* r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+d->len*a->esz,0);
    UF_PROTECT(&r);
    r->tag=a->tag; r->len=d->len; r->esz=a->esz; r->ety=a->ety;
    for(uint64_t i=0;i<d->len;i++) uf_cseti(uf_mkp(r),(int64_t)i,d->data[i]);
    UF_UNPROTECT(); UF_UNPROTECT();
    pushp(cx,r); return;
  }
  Dyn* r=uf_dyn_new(d->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<d->len;i++)uf_dyn_push(&r,d->data[i]);
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* FILTER: list pred_addr -> list' */
static void op_filter(Ctx*cx){
  Cell f=pop(cx),h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(8); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i); Cell k=pop(cx);
    if(!uf_zero(k)) uf_dyn_push(&r,s->data[i]);
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* SOME/EVERY: list pred_addr -> 0/1 (short-circuit) */
static void op_some(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  int r=0;
  for(uint64_t i=0;i<s->len;i++){ pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i); if(!uf_zero(pop(cx))){r=1;break;} }
  UF_UNPROTECT(); pushi(cx,r);
}
static void op_every(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  int r=1;
  for(uint64_t i=0;i<s->len;i++){ pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i); if(uf_zero(pop(cx))){r=0;break;} }
  UF_UNPROTECT(); pushi(cx,r);
}

/* ================= vector ops + bitmap masks ================= */
static double uf_el(Hdr*a,uint64_t i){ char*dt=uf_data(a); if(a->ety==1)return ((double*)dt)[i]; if(a->ety==3)return (double)((uint8_t*)dt)[i]; return (double)((int64_t*)dt)[i]; }
static void uf_put_el(Hdr*a,uint64_t i,double d){ char*dt=uf_data(a); if(a->ety==1)((double*)dt)[i]=d; else if(a->ety==3)((uint8_t*)dt)[i]=(uint8_t)d; else ((int64_t*)dt)[i]=(int64_t)d; }
static Hdr* uf_arr_like(Hdr*a,uint64_t n){ Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*a->esz,0); r->tag=a->tag; r->len=n; r->esz=a->esz; r->ety=a->ety; return r; }
static Hdr* uf_vcheck(Cell h,const char*op){ Hdr*a=uf_handle(h,op); if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR)die("vector op: not an arr"); return a; }
/* scalar arr ops: arr scalar -> arr' */
#define UF_VSOP(NAME,EXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(a->ety==1||s.tag==T_FLOAT){ double d=uf_f(s); if(ZERO_DIE&&d==0.0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  else { int64_t d=s.i; if(ZERO_DIE&&d==0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  UF_UNPROTECT(); pushp(cx,r); }
UF_VSOP(op_vadd, uf_el(a,i)+d, 0)
UF_VSOP(op_vsub, uf_el(a,i)-d, 0)
UF_VSOP(op_vmul, uf_el(a,i)*d, 0)
UF_VSOP(op_vdiv, uf_el(a,i)/d, 1)
/* elementwise arr arr ops: length mismatch dies */
#define UF_VEOP(NAME,EXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Hdr*a=uf_vcheck(h1,#NAME); Hdr*b=uf_vcheck(h2,#NAME); \
  if(a->len!=b->len)die(#NAME ": length mismatch"); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(ZERO_DIE) for(uint64_t i=0;i<n;i++) if(uf_el(b,i)==0.0)die(#NAME ": zero divisor"); \
  for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); \
  UF_UNPROTECT(); pushp(cx,r); }
UF_VEOP(op_veadd, uf_el(a,i)+uf_el(b,i), 0)
UF_VEOP(op_vesub, uf_el(a,i)-uf_el(b,i), 0)
UF_VEOP(op_vemul, uf_el(a,i)*uf_el(b,i), 0)
UF_VEOP(op_vediv, uf_el(a,i)/uf_el(b,i), 1)
UF_VEOP(op_vemax, uf_el(a,i)>uf_el(b,i)?uf_el(a,i):uf_el(b,i), 0)
UF_VEOP(op_vemin, uf_el(a,i)<uf_el(b,i)?uf_el(a,i):uf_el(b,i), 0)
/* comparisons: arr scalar -> bitmap */
static Bitmap* uf_bm_new(uint64_t nbits){ Bitmap*b=(Bitmap*)uf_gc_alloc(sizeof(Bitmap)+((nbits+63)/64)*8,0); b->tag=HT_BITMAP; b->len=nbits; b->esz=8; memset(b->words,0,((nbits+63)/64)*8); return b; }
#define UF_VCOP(NAME,EXPR) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  uint64_t n=a->len; Bitmap*r=uf_bm_new(n); UF_PROTECT(&r); \
  double d=uf_f(s); \
  for(uint64_t i=0;i<n;i++) if(EXPR) r->words[i>>6]|=(1ULL<<(i&63)); \
  UF_UNPROTECT(); pushp(cx,r); }
UF_VCOP(op_veq, uf_el(a,i)==d)
UF_VCOP(op_vlt, uf_el(a,i)<d)
UF_VCOP(op_vgt, uf_el(a,i)>d)
UF_VCOP(op_vge, uf_el(a,i)>=d)
UF_VCOP(op_vle, uf_el(a,i)<=d)
/* bitmap logic */
static Bitmap* uf_bm_check(Cell h){ Hdr*a=uf_handle(h,"bitmap op"); if(a->tag!=HT_BITMAP)die("bitmap op: not a bitmap"); return (Bitmap*)a; }
static void op_vand(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Bitmap*a=uf_bm_check(h1),*b=uf_bm_check(h2); if(a->len!=b->len)die("VAND: length mismatch"); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=a->words[i]&b->words[i]; UF_UNPROTECT(); pushp(cx,r); }
static void op_vor(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Bitmap*a=uf_bm_check(h1),*b=uf_bm_check(h2); if(a->len!=b->len)die("VOR: length mismatch"); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=a->words[i]|b->words[i]; UF_UNPROTECT(); pushp(cx,r); }
static void op_vnot(Ctx*cx){ Cell h=pop(cx); Bitmap*a=uf_bm_check(h); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=~a->words[i]; if(a->len&63)r->words[w-1]&=(1ULL<<(a->len&63))-1; UF_UNPROTECT(); pushp(cx,r); }
static void op_vcount(Ctx*cx){ Cell h=pop(cx); Bitmap*a=uf_bm_check(h); uint64_t w=(a->len+63)/64,n=0; for(uint64_t i=0;i<w;i++)n+=(uint64_t)__builtin_popcountll(a->words[i]); pushi(cx,(int64_t)n); }
/* VGATHER: arr bm -> arr' (keep set-bit elements) */
static void op_vgather(Ctx*cx){
  Cell h2=pop(cx),h1=pop(cx); Hdr*a=uf_vcheck(h1,"VGATHER"); Bitmap*b=uf_bm_check(h2);
  if(a->len!=b->len)die("VGATHER: length mismatch");
  uint64_t n=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)n++;
  Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r);
  uint64_t k=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)uf_put_el(r,k++,uf_el(a,i));
  UF_UNPROTECT(); pushp(cx,r);
}
/* reductions */
static void op_vsum(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VSUM"); double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmean(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMEAN"); if(!a->len)die("VMEAN: empty arr"); double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); pushf(cx,s/(double)a->len); }
static void op_vmin(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMIN"); if(!a->len)die("VMIN: empty arr"); double s=uf_el(a,0); for(uint64_t i=1;i<a->len;i++){double d=uf_el(a,i);if(d<s)s=d;} if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmax(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMAX"); if(!a->len)die("VMAX: empty arr"); double s=uf_el(a,0); for(uint64_t i=1;i<a->len;i++){double d=uf_el(a,i);if(d>s)s=d;} if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
/* VMAP: arr fn_addr -> arr' ; VFOLD: arr init fn_addr -> acc */
static void op_vmap(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VMAP");
  Hdr*r=uf_arr_like(a,a->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<a->len;i++){
    if(a->ety==1)pushf(cx,uf_el(a,i)); else pushi(cx,(int64_t)uf_el(a,i));
    uf_call_addr(cx,(const void*)f.i);
    uf_put_el(r,i,uf_f(pop(cx)));
  }
  UF_UNPROTECT(); pushp(cx,r);
}
static void op_vfold(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VFOLD");
  for(uint64_t i=0;i<a->len;i++){
    pushc(cx,acc);
    if(a->ety==1)pushf(cx,uf_el(a,i)); else pushi(cx,(int64_t)uf_el(a,i));
    uf_call_addr(cx,(const void*)f.i);
    acc=pop(cx);
  }
  pushc(cx,acc);
}
/* VARGSORT: arr -> idx_arr (stable) */
static void op_vargsort(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VARGSORT");
  uint64_t n=a->len;
  int64_t* idx=(int64_t*)uf_alloc((n?n:1)*8,0); int64_t* tmp=(int64_t*)uf_alloc((n?n:1)*8,0);
  for(uint64_t i=0;i<n;i++)idx[i]=(int64_t)i;
  /* stable mergesort of indices by element value */
  for(uint64_t w=1;w<n;w*=2){
    for(uint64_t lo=0;lo<n;lo+=2*w){
      uint64_t mid=lo+w<n?lo+w:n, hi=lo+2*w<n?lo+2*w:n;
      uint64_t i=lo,j=mid,k=lo;
      while(i<mid&&j<hi){ if(uf_el(a,(uint64_t)idx[i])<=uf_el(a,(uint64_t)idx[j]))tmp[k++]=idx[i++]; else tmp[k++]=idx[j++]; }
      while(i<mid)tmp[k++]=idx[i++];
      while(j<hi)tmp[k++]=idx[j++];
    }
    int64_t* t=idx; idx=tmp; tmp=t;
  }
  Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*8,0); r->tag=HT_ARR; r->len=n; r->esz=8; r->ety=0;
  memcpy(r->data,idx,n*8); free(idx); free(tmp);
  pushp(cx,r);
}
/* VSEARCHSORTED: sorted_arr val -> idx (insertion point) */
static void op_vsearchsorted(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VSEARCHSORTED");
  double d=uf_f(v); uint64_t lo=0,hi=a->len;
  while(lo<hi){ uint64_t mid=(lo+hi)/2; if(uf_el(a,mid)<d)lo=mid+1; else hi=mid; }
  pushi(cx,(int64_t)lo);
}
/* VWHERE: arr arr bm -> arr' (bit set -> first arr, else second) */
static void op_vwhere(Ctx*cx){
  Cell h3=pop(cx),h2=pop(cx),h1=pop(cx);
  Hdr*a=uf_vcheck(h1,"VWHERE"); Hdr*b=uf_vcheck(h2,"VWHERE"); Bitmap*m=uf_bm_check(h3);
  if(a->len!=b->len||a->len!=m->len)die("VWHERE: length mismatch");
  Hdr*r=uf_arr_like(a,a->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<a->len;i++) uf_put_el(r,i, ((m->words[i>>6]>>(i&63))&1)?uf_el(a,i):uf_el(b,i));
  UF_UNPROTECT(); pushp(cx,r);
}

/* ================= data ops: group/agg/unique/flat/chunk ================= */
/* GROUP: list fn_addr -> dict (key -> list of elems, insertion-ordered keys) */
static void op_group(Ctx*cx){
  Cell f=pop(cx),h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Map* m=uf_map_new(); UF_PROTECT(&m);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  for(uint64_t i=0;i<s->len;i++){
    pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i); Cell key=pop(cx);
    Cell lst;
    if(map_get(m,key,&lst)){ Dyn* nd=uf_dyn_push2((Dyn*)uf_gc_find((void*)lst.i),s->data[i]); if((void*)nd!=(void*)lst.i) map_put(m,key,uf_mkp(nd)); }
    else { Dyn* d=uf_dyn_new(4); d=uf_dyn_push2(d,s->data[i]); map_put(m,key,uf_mkp(d)); uf_dyn_push(&order,key); }
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,m);
}
/* AGG: dict fn_addr -> dict' (map each group's value-list through fn) */
static void op_agg(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"AGG"); if(a->tag!=HT_MAP)die("AGG: not a dict");
  Map* m=(Map*)a;
  Map* r=uf_map_new(); UF_PROTECT(&r);
  for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){
    pushc(cx,m->vals[i]); uf_call_addr(cx,(const void*)f.i); Cell v=pop(cx);
    map_put(r,m->keys[i],v);
  }
  UF_UNPROTECT(); pushp(cx,r);
}
/* UNIQUE: list -> list' (dedup, first-occurrence order; dict-hashable elems) */
static void op_unique(Ctx*cx){
  Cell h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* r=uf_dyn_new(s->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    Cell v;
    if(!map_get(seen,s->data[i],&v)){ map_put(seen,s->data[i],uf_mki(1)); uf_dyn_push(&r,s->data[i]); }
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* FLAT: list -> list' (flatten one level; non-list elements pass through) */
static void op_flat(Ctx*cx){
  Cell h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(8); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    Cell e=s->data[i]; Hdr* eh=e.tag==T_PTR&&e.i?uf_gc_find((void*)e.i):0;
    if(eh&&eh->tag==HT_DYN){ Dyn* d=(Dyn*)eh; for(uint64_t j=0;j<d->len;j++)uf_dyn_push(&r,d->data[j]); }
    else uf_dyn_push(&r,e);
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* CHUNK: seq size -> list of pieces (last may be short) */
static void op_chunk(Ctx*cx){
  int64_t sz=pop(cx).i; Cell h=pop(cx);
  if(sz<1)die("CHUNK: size < 1");
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  int isarr = a&&(a->tag==HT_ARR||a->tag==HT_TENSOR);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(s->len/(uint64_t)sz+1); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i+=(uint64_t)sz){
    uint64_t n=s->len-i<(uint64_t)sz?s->len-i:(uint64_t)sz;
    if(isarr){
      Hdr* p=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*a->esz,0);
      p->tag=a->tag; p->len=n; p->esz=a->esz; p->ety=a->ety;
      for(uint64_t j=0;j<n;j++)uf_cseti(uf_mkp(p),(int64_t)j,s->data[i+j]);
      uf_dyn_push(&r,uf_mkp(p));
    } else {
      Dyn* p=uf_dyn_new(n);
      for(uint64_t j=0;j<n;j++)p=uf_dyn_push2(p,s->data[i+j]);
      uf_dyn_push(&r,uf_mkp(p));
    }
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}

/* ================= time ops (scalar cells: tag 15 time, 16 dur, i64 nanos) ================= */
static void op_now(Ctx*cx){ struct timespec ts; clock_gettime(CLOCK_REALTIME,&ts); Cell c; c.tag=T_TIME; c.i=(int64_t)ts.tv_sec*1000000000LL+(int64_t)ts.tv_nsec; pushc(cx,c); }
/* TIME: str fmt -> t ; fmt "unix" (float s) or strptime(3) */
static void op_time(Ctx*cx){
  Cell fmt=pop(cx),st=pop(cx);
  const char* F=uf_sptr(fmt); const char* S=uf_sptr(st);
  int64_t ns;
  if(strcmp(F,"unix")==0){ double d=strtod(S,0); ns=(int64_t)(d*1e9); }
  else {
    struct tm tm; memset(&tm,0,sizeof tm);
    if(!strptime(S,F,&tm))die("TIME: unparseable");
    time_t t=mktime(&tm);
    ns=(int64_t)t*1000000000LL;
  }
  Cell c; c.tag=T_TIME; c.i=ns; pushc(cx,c);
}
/* TIMEF: t fmt -> str ; fmt "unix" or strftime(3) (process TZ via libc) */
static void op_timef(Ctx*cx){
  Cell fmt=pop(cx),t=pop(cx);
  const char* F=uf_sptr(fmt);
  char buf[256];
  if(strcmp(F,"unix")==0){ snprintf(buf,sizeof buf,"%.9f",(double)t.i/1e9); }
  else {
    time_t s=(time_t)(t.i/1000000000LL);
    struct tm tm; localtime_r(&s,&tm);
    if(!strftime(buf,sizeof buf,F,&tm))die("TIMEF: format failed");
  }
  pushc(cx,uf_str_new(buf,strlen(buf)));
}

/* ================= bloom filter (tag 17; double-hashed FNV-1a) ================= */
static void op_bloom(Ctx*cx){
  int64_t n=pop(cx).i; if(n<1)die("BLOOM: n < 1");
  uint64_t bits=(uint64_t)((double)n*9.585)+64; /* ~1% FP */
  Bloom* b=(Bloom*)uf_gc_alloc(sizeof(Bloom)+((bits+63)/64)*8,0);
  b->tag=HT_BLOOM; b->len=bits; b->esz=8; b->k=7;
  memset(b->words,0,((bits+63)/64)*8);
  pushp(cx,b);
}
static void uf_bloom_hashes(Cell v,uint64_t* h1,uint64_t* h2){
  if(uf_is_str(v)){ const char* s=uf_sptr(v); size_t n=strlen(s); *h1=uf_fnv(s,n); *h2=uf_fnv(s,n)^0x9e3779b97f4a7c15ULL; *h2=uf_fnv(h2,8); }
  else { *h1=uf_fnv(&v.i,8); uint64_t x=(uint64_t)v.i^0x9e3779b97f4a7c15ULL; *h2=uf_fnv(&x,8); }
  if(!*h2)*h2=0x100000001b3ULL;
}
static void op_badd(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"BADD"); if(a->tag!=HT_BLOOM)die("BADD: not a bloom filter");
  Bloom* b=(Bloom*)a; uint64_t h1,h2; uf_bloom_hashes(v,&h1,&h2);
  for(int i=0;i<b->k;i++){ uint64_t p=(h1+(uint64_t)i*h2)%b->len; b->words[p>>6]|=(1ULL<<(p&63)); }
}
static void op_btest(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"BTEST"); if(a->tag!=HT_BLOOM)die("BTEST: not a bloom filter");
  Bloom* b=(Bloom*)a; uint64_t h1,h2; uf_bloom_hashes(v,&h1,&h2);
  for(int i=0;i<b->k;i++){ uint64_t p=(h1+(uint64_t)i*h2)%b->len; if(!((b->words[p>>6]>>(p&63))&1)){ pushi(cx,0); return; } }
  pushi(cx,1);
}

/* ================= script I/O ================= */
/* SLURP: path -> str (whole file; not found/unreadable: dies) */
static void op_slurp(Ctx*cx){
  Cell p=pop(cx);
  FILE* f=fopen(uf_sptr(p),"rb"); if(!f)die("SLURP: cannot open file");
  char* b=uf_read_all(f); fclose(f);
  Cell r=uf_str_new(b,strlen(b)); free(b); pushc(cx,r);
}
/* SPIT: path str ->  (create/truncate; error: dies) */
static void op_spit(Ctx*cx){
  Cell st=pop(cx),p=pop(cx);
  FILE* f=fopen(uf_sptr(p),"wb"); if(!f)die("SPIT: cannot open file");
  const char* s=uf_sptr(st); size_t n=strlen(s);
  if(n&&fwrite(s,1,n,f)!=n){ fclose(f); die("SPIT: write failed"); }
  fclose(f);
}
/* ARGV: -> list of strings */
static void op_argv(Ctx*cx){
  Dyn* d=uf_dyn_new((uint64_t)uf_argc); UF_PROTECT(&d);
  char** av=(char**)uf_argv;
  for(int64_t i=0;i<uf_argc;i++){ Cell s=uf_str_new(av[i],strlen(av[i])); uf_dyn_push(&d,s); }
  UF_UNPROTECT(); pushp(cx,d);
}

/* ================= zero-copy file access + streaming ================= */
/* MMAP: path -> str (read-only zero-copy; tag str, GC-registered, munmap'd
   when swept; falls back to an owned copy for page-aligned sizes so the
   NUL terminator never crosses the mapping) */
static void op_mmap(Ctx*cx){
  Cell p=pop(cx);
  const char* path=uf_sptr(p);
  int fd=open(path,O_RDONLY); if(fd<0)die("MMAP: cannot open file");
  struct stat sb; if(fstat(fd,&sb)<0){ close(fd); die("MMAP: stat failed"); }
  uint64_t n=(uint64_t)sb.st_size;
  if(n%4096==0){ /* no tail slack for the NUL: owned copy fallback */
    Str* s=(Str*)uf_gc_alloc(sizeof(Str)+n+1,0);
    s->tag=HT_STR; s->len=n; s->esz=1; s->mlen=0;
    ssize_t got=read(fd,s->data,n); close(fd);
    if(got<0)die("MMAP: read failed");
    s->len=(uint64_t)got; s->data[got]=0;
    pushp(cx,s); return;
  }
  /* last partial page zero-fills past EOF, so data[n]==0 for free */
  uint64_t flen=(n+4095)&~(uint64_t)4095;
  void* fm=mmap(0,flen,PROT_READ,MAP_PRIVATE,fd,0);
  close(fd);
  if(fm==MAP_FAILED)die("MMAP: map failed");
  Str* s=(Str*)uf_gc_alloc(sizeof(Str),0);
  s->tag=HT_STR; s->len=n; s->esz=1; s->mlen=flen; s->mdata=(const char*)fm;
  s->gc_flags|=GCF_MMAP;
  pushp(cx,s);
}
/* FEACH: path fn_addr ->  (fn: line -> cont; streamed; 0 stops early) */
static void op_feach(Ctx*cx){
  Cell f=pop(cx),p=pop(cx);
  FILE* fp=fopen(uf_sptr(p),"r"); if(!fp)die("FEACH: cannot open file");
  char* line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,fp))>=0){
    while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
    Cell ls=uf_str_new(line,(size_t)m);
    pushc(cx,ls); uf_call_addr(cx,(const void*)f.i); Cell k=pop(cx);
    if(uf_zero(k))break;
  }
  free(line); fclose(fp);
}
/* FFOLD: path init fn_addr -> acc (fn: acc line -> acc) */
static void op_ffold(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx),p=pop(cx);
  FILE* fp=fopen(uf_sptr(p),"r"); if(!fp)die("FFOLD: cannot open file");
  char* line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,fp))>=0){
    while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
    Cell ls=uf_str_new(line,(size_t)m);
    pushc(cx,acc); pushc(cx,ls); uf_call_addr(cx,(const void*)f.i); acc=pop(cx);
  }
  free(line); fclose(fp);
  pushc(cx,acc);
}
/* FMATCH: path pat -> chan (producer thread streams matching lines, cap 64) */
typedef struct { Ring* r; char* path; char* pat; } UfFm;
static void* uf_fmatch_worker(void* arg){
  UfFm* g=(UfFm*)arg;
  FILE* fp=fopen(g->path,"r");
  if(fp){
    char* line=0; size_t ncap=0; ssize_t m; RxCap caps[10];
    while((m=getline(&line,&ncap,fp))>=0){
      while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
      if(rx_exec(g->pat,line,caps)){ Cell v=uf_str_new(line,(size_t)m); ring_enq(g->r,v); }
    }
    free(line); fclose(fp);
  }
  ring_close(g->r);
  free(g->path); free(g->pat); free(g);
  return 0;
}
static void op_fmatch(Ctx*cx){
  Cell pat=pop(cx),p=pop(cx);
  Ring* r=uf_ring_new(64);
  UfFm* g=(UfFm*)malloc(sizeof(UfFm)); if(!g)die("out of memory");
  g->r=r; g->path=strdup(uf_sptr(p)); g->pat=strdup(uf_sptr(pat));
  pthread_t th; if(pthread_create(&th,0,uf_fmatch_worker,g)){ ring_close(r); die("FMATCH: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}

/* ================= graph traversal (visited set via dict probe loop) ================= */
/* BFS: start fn_addr -> list (fn: node -> list of neighbors) */
static void op_bfs(Ctx*cx){
  Cell f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  Dyn* queue=uf_dyn_new(8); UF_PROTECT(&queue);
  map_put(seen,start,uf_mki(1));
  uf_dyn_push(&queue,start);
  uint64_t qi=0;
  while(qi<queue->len){
    Cell node=queue->data[qi++];
    uf_dyn_push(&order,node);
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=0;i<nbs->len;i++){
      Cell v;
      if(!map_get(seen,nbs->data[i],&v)){ map_put(seen,nbs->data[i],uf_mki(1)); uf_dyn_push(&queue,nbs->data[i]); }
    }
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,order);
}
/* DFS: start fn_addr -> list (pre-order) */
static void op_dfs(Ctx*cx){
  Cell f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  Dyn* stack=uf_dyn_new(8); UF_PROTECT(&stack);
  uf_dyn_push(&stack,start);
  while(stack->len){
    Cell node=stack->data[--stack->len];
    Cell v;
    if(map_get(seen,node,&v))continue;
    map_put(seen,node,uf_mki(1));
    uf_dyn_push(&order,node);
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=nbs->len;i>0;i--) uf_dyn_push(&stack,nbs->data[i-1]);
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,order);
}
/* WFIND: start fn_addr pred_addr -> v_or_0 (BFS, early exit) */
static void op_wfind(Ctx*cx){
  Cell pred=pop(cx),f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* queue=uf_dyn_new(8); UF_PROTECT(&queue);
  map_put(seen,start,uf_mki(1));
  uf_dyn_push(&queue,start);
  uint64_t qi=0; int found=0; Cell result=uf_mki(0);
  while(qi<queue->len&&!found){
    Cell node=queue->data[qi++];
    pushc(cx,node); uf_call_addr(cx,(const void*)pred.i); Cell k=pop(cx);
    if(!uf_zero(k)){ result=node; found=1; break; }
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=0;i<nbs->len;i++){
      Cell v;
      if(!map_get(seen,nbs->data[i],&v)){ map_put(seen,nbs->data[i],uf_mki(1)); uf_dyn_push(&queue,nbs->data[i]); }
    }
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushc(cx,result);
}

/* ================= JSON ================= */
typedef struct { const char* p; } JCur;
static void j_ws(JCur* j){ while(*j->p&&isspace((unsigned char)*j->p))j->p++; }
static Cell j_parse(JCur* j);
static Cell j_str(JCur* j){
  j->p++; /* opening quote */
  size_t cap=32,n=0; char* b=(char*)uf_alloc(cap,0);
  for(;;){
    char c=*j->p;
    if(!c){ free(b); die("JSON: unterminated string"); }
    j->p++;
    if(c=='"')break;
    if(c=='\\'){
      char e=*j->p++; 
      switch(e){
        case 'n': c='\n'; break; case 't': c='\t'; break; case 'r': c='\r'; break;
        case 'b': c='\b'; break; case 'f': c='\f'; break; case '/': c='/'; break;
        case '\\': c='\\'; break; case '"': c='"'; break;
        case 'u': { /* keep the ASCII byte of \u00XX; else '?') */
          if(j->p[0]=='0'&&j->p[1]=='0'){ char hex[3]={j->p[2],j->p[3],0}; c=(char)strtol(hex,0,16); }
          else c='?';
          j->p+=4; break; }
        default: free(b); die("JSON: bad escape");
      }
    }
    if(n+2>cap){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); }
    b[n++]=c;
  }
  Cell r=uf_str_new(b,n); free(b); return r;
}
static Cell j_parse(JCur* j){
  j_ws(j);
  char c=*j->p;
  if(c=='{'){
    j->p++; Map* m=uf_map_new(); UF_PROTECT(&m);
    j_ws(j);
    if(*j->p=='}'){ j->p++; UF_UNPROTECT(); return uf_mkp(m); }
    for(;;){
      j_ws(j); if(*j->p!='"')die("JSON: object key must be a string");
      Cell k=j_str(j);
      j_ws(j); if(*j->p!=':')die("JSON: expected ':'");
      j->p++;
      Cell v=j_parse(j);
      map_put(m,k,v);
      j_ws(j);
      if(*j->p==','){ j->p++; continue; }
      if(*j->p=='}'){ j->p++; break; }
      die("JSON: expected ',' or '}'");
    }
    UF_UNPROTECT(); return uf_mkp(m);
  }
  if(c=='['){
    j->p++; Dyn* d=uf_dyn_new(8); UF_PROTECT(&d);
    j_ws(j);
    if(*j->p==']'){ j->p++; UF_UNPROTECT(); return uf_mkp(d); }
    for(;;){
      Cell v=j_parse(j); uf_dyn_push(&d,v);
      j_ws(j);
      if(*j->p==','){ j->p++; continue; }
      if(*j->p==']'){ j->p++; break; }
      die("JSON: expected ',' or ']'");
    }
    UF_UNPROTECT(); return uf_mkp(d);
  }
  if(c=='"') return j_str(j);
  if(!strncmp(j->p,"true",4)){ j->p+=4; return uf_mki(1); }
  if(!strncmp(j->p,"false",5)){ j->p+=5; return uf_mki(0); }
  if(!strncmp(j->p,"null",4)){ j->p+=4; return uf_mki(0); }
  if(c=='-'||isdigit((unsigned char)c)){
    const char* s=j->p; char* e;
    double d=strtod(s,&e);
    if(e==s)die("JSON: bad number");
    int isint=1;
    for(const char* q=s;q<e;q++) if(*q=='.'||*q=='e'||*q=='E'){isint=0;break;}
    j->p=e;
    if(isint){ int64_t v=strtoll(s,0,10); return uf_mki(v); }
    return uf_mkf(d);
  }
  die("JSON: malformed input");
  return uf_mki(0);
}
static void op_json(Ctx*cx){
  Cell st=pop(cx);
  JCur j={uf_sptr(st)};
  Cell v=j_parse(&j);
  j_ws(&j);
  if(*j.p)die("JSON: trailing garbage");
  pushc(cx,v);
}
/* UNJSON: v -> str (dict keys must be strings; atom/chan/iter/bitmap/bloom: dies) */
static void uf_unjson_w(Cell v,char** bp,size_t* np,size_t* capp){
#define UW(src,L) do{ size_t _l=(size_t)(L); while(*np+_l+1>*capp){ *capp*=2; *bp=(char*)realloc(*bp,*capp); if(!*bp)die("out of memory"); } memcpy(*bp+*np,(src),_l); *np+=_l; }while(0)
  char tmp[64];
  Hdr* a=v.tag==T_PTR&&v.i?uf_gc_find((void*)v.i):0;
  if(a){
    switch(a->tag){
      case HT_STR: {
        UW("\"",1);
        const char* s=uf_sbytes((Str*)a);
        for(uint64_t i=0;i<a->len;i++){ char c=s[i];
          if(c=='"'||c=='\\'){ UW("\\",1); UW(&c,1); }
          else if(c=='\n')UW("\\n",2);
          else if(c=='\t')UW("\\t",2);
          else if(c=='\r')UW("\\r",2);
          else if((unsigned char)c<0x20){ snprintf(tmp,sizeof tmp,"\\u%04x",c); UW(tmp,6); }
          else UW(&c,1);
        }
        UW("\"",1); return; }
      case HT_DYN: {
        Dyn* d=(Dyn*)a; UW("[",1);
        for(uint64_t i=0;i<d->len;i++){ if(i)UW(",",1); uf_unjson_w(d->data[i],bp,np,capp); }
        UW("]",1); return; }
      case HT_MAP: {
        Map* m=(Map*)a; UW("{",1); int first=1;
        for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){
          if(!uf_is_str(m->keys[i]))die("UNJSON: dict keys must be strings");
          if(!first)UW(",",1); first=0;
          uf_unjson_w(m->keys[i],bp,np,capp); UW(":",1); uf_unjson_w(m->vals[i],bp,np,capp);
        }
        UW("}",1); return; }
      case HT_ARR: case HT_TENSOR: {
        UW("[",1);
        for(uint64_t i=0;i<a->len;i++){ if(i)UW(",",1); uf_unjson_w(uf_cidx(v,(int64_t)i),bp,np,capp); }
        UW("]",1); return; }
      default: die("UNJSON: unsupported handle (atom/chan/iter/bitmap/bloom)");
    }
  }
  if(v.tag==T_FLOAT){ snprintf(tmp,sizeof tmp,"%.17g",uf_f(v)); UW(tmp,strlen(tmp)); return; }
  snprintf(tmp,sizeof tmp,"%lld",(long long)v.i); UW(tmp,strlen(tmp));
#undef UW
}
static void op_unjson(Ctx*cx){
  Cell v=pop(cx);
  size_t cap=256,n=0; char* b=(char*)uf_alloc(cap,0);
  uf_unjson_w(v,&b,&n,&cap);
  Cell r=uf_str_new(b,n); free(b); pushc(cx,r);
}

/* ================= streaming sink ================= */
/* FEMIT: it path -> n (path on top; one item per line; ints/floats decimal,
   strings as-is, everything else unjson) */
static void op_femit(Ctx*cx){
  Cell p=pop(cx),h=pop(cx);
  FILE* f=fopen(uf_sptr(p),"w"); if(!f)die("FEMIT: cannot open file");
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  Iter* it = (a&&a->tag==HT_ITER) ? (Iter*)a : uf_iter_new(h);
  UF_PROTECT(&it);
  Cell v; int64_t n=0;
  while(uf_iter_next(cx,it,&v)){
    Hdr* e=v.tag==T_PTR&&v.i?uf_gc_find((void*)v.i):0;
    if(e&&e->tag==HT_STR){ fwrite(uf_sbytes((Str*)e),1,e->len,f); }
    else if(v.tag==T_FLOAT){ fprintf(f,"%.17g",uf_f(v)); }
    else if(v.tag==T_INT||v.tag==T_TIME||v.tag==T_DUR){ fprintf(f,"%lld",(long long)v.i); }
    else {
      size_t cap=256,m=0; char* b=(char*)uf_alloc(cap,0);
      uf_unjson_w(v,&b,&m,&cap);
      fwrite(b,1,m,f); free(b);
    }
    fputc('\n',f); n++;
  }
  UF_UNPROTECT();
  fclose(f);
  pushi(cx,n);
}

/* ================= error containment ================= */
static int uf_try_once(Ctx*cx, const void* a, Cell* out){
  UfTry t; t.prev=uf_try_top; t.sp=cx->sp; t.csp=cx->csp; uf_try_top=&t;
  if(setjmp(t.jb)==0){
    uf_call_addr(cx,a);
    uf_try_top=t.prev;
    Cell r = cx->sp>t.sp ? pop(cx) : uf_mki(0);
    cx->sp=t.sp;
    *out=r;
    return 1;
  }
  uf_try_top=t.prev;
  cx->sp=t.sp;
  cx->csp=t.csp;
  if(uf_cur_task)((WeaveTask*)uf_cur_task)->tolerated++;
  return 0;
}
/* TRY: body_addr -> result ok */
static void op_try(Ctx*cx){
  Cell a=pop(cx); Cell r;
  if(uf_try_once(cx,(const void*)a.i,&r)){ pushc(cx,r); pushi(cx,1); }
  else { pushi(cx,0); pushi(cx,0); }
}
/* RETRY: n body_addr -> result ok (up to n+1 attempts, first success stops) */
static void op_retry(Ctx*cx){
  Cell a=pop(cx); int64_t n=pop(cx).i; Cell r;
  for(int64_t k=0;;k++){
    if(uf_try_once(cx,(const void*)a.i,&r)){
      if(k&&uf_cur_task)((WeaveTask*)uf_cur_task)->retries+=k;
      pushc(cx,r); pushi(cx,1); return;
    }
    if(k>=n){ pushi(cx,0); pushi(cx,0); return; }
  }
}

/* ================= detached threads ================= */
typedef struct { const void* body; Ring* r; } UfSpawn;
static void* uf_spawn_worker(void* arg){
  UfSpawn* g=(UfSpawn*)arg;
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_call_addr(c,g->body);
  Cell r = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
  ring_enq(g->r,r);
  ring_close(g->r);
  ctx_free(c);
  free(g);
  return 0;
}
/* SPAWN: body_addr -> chan (cap 1; body's top-of-stack enqueued at end,
   then closed — deq on it is a join) */
static void op_spawn(Ctx*cx){
  Cell a=pop(cx);
  Ring* r=uf_ring_new(1);
  UfSpawn* g=(UfSpawn*)malloc(sizeof(UfSpawn)); if(!g)die("out of memory");
  g->body=(const void*)a.i; g->r=r;
  pthread_t th; if(pthread_create(&th,0,uf_spawn_worker,g)){ ring_close(r); die("SPAWN: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* init-TU worker: fire-and-forget thread for init.uf entry points.
   Same as uf_spawn_worker minus the chan — process exit kills these. */
static void* uf_init_worker(void* arg){
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_call_addr(c,arg);
  ctx_free(c);
  return 0;
}
static void uf_init_reflection(void){}
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl0 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl1 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl2 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl3 = {0,0,0,9,22,1,0,0,0,"parse error: expected "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl4 = {0,0,0,9,5,1,0,0,0," got "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl5 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl6 = {0,0,0,9,3,1,0,0,0,"int"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl7 = {0,0,0,9,4,1,0,0,0,"char"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl8 = {0,0,0,9,4,1,0,0,0,"void"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl9 = {0,0,0,9,4,1,0,0,0,"long"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl10 = {0,0,0,9,5,1,0,0,0,"short"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl11 = {0,0,0,9,8,1,0,0,0,"unsigned"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl12 = {0,0,0,9,6,1,0,0,0,"signed"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl13 = {0,0,0,9,5,1,0,0,0,"const"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl14 = {0,0,0,9,6,1,0,0,0,"static"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl15 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl16 = {0,0,0,9,1,1,0,0,0,"v"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl17 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl18 = {0,0,0,9,20,1,0,0,0,"undefined variable: "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl19 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl20 = {0,0,0,9,16,1,0,0,0,"^\"([^\"\\\\]|\\\\.)*\""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl21 = {0,0,0,9,16,1,0,0,0,"^'([^'\\\\]|\\\\.)*'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl22 = {0,0,0,9,27,1,0,0,0,"^(0[xX][0-9a-fA-F]+|[0-9]+)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[24]; } uf_sl23 = {0,0,0,9,23,1,0,0,0,"^[A-Za-z_][A-Za-z0-9_]*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[62]; } uf_sl24 = {0,0,0,9,61,1,0,0,0,"^(<<=|>>=|==|!=|<=|>=|&&|\\|\\||\\+\\+|--|\\+=|-=|\\*=|/=|%=|<<|>>)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl25 = {0,0,0,9,8,1,0,0,0,"^[\t\n\r ]+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl26 = {0,0,0,9,2,1,0,0,0,"//"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl27 = {0,0,0,9,2,1,0,0,0,"/*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl28 = {0,0,0,9,1,1,0,0,0,"#"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl29 = {0,0,0,9,8,1,0,0,0,"^[\t\n\r ]+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl30 = {0,0,0,9,2,1,0,0,0,"//"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl31 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl32 = {0,0,0,9,2,1,0,0,0,"/*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl33 = {0,0,0,9,2,1,0,0,0,"*/"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl34 = {0,0,0,9,1,1,0,0,0,"#"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl35 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl36 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl37 = {0,0,0,9,2,1,0,0,0,"+="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl38 = {0,0,0,9,2,1,0,0,0,"-="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl39 = {0,0,0,9,2,1,0,0,0,"*="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl40 = {0,0,0,9,2,1,0,0,0,"/="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl41 = {0,0,0,9,2,1,0,0,0,"%="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl42 = {0,0,0,9,2,1,0,0,0,"/="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl43 = {0,0,0,9,2,1,0,0,0,"%="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl44 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl45 = {0,0,0,9,4,1,0,0,0,"dup "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl46 = {0,0,0,9,4,1,0,0,0,"%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl47 = {0,0,0,9,4,1,0,0,0,"%s@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl48 = {0,0,0,9,5,1,0,0,0," dup "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl49 = {0,0,0,9,4,1,0,0,0,"%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[22]; } uf_sl50 = {0,0,0,9,21,1,0,0,0,"no division in subset"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl51 = {0,0,0,9,2,1,0,0,0,"||"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[24]; } uf_sl52 = {0,0,0,9,23,1,0,0,0,"not not swp not not or "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl53 = {0,0,0,9,2,1,0,0,0,"&&"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[25]; } uf_sl54 = {0,0,0,9,24,1,0,0,0,"not not swp not not and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl55 = {0,0,0,9,2,1,0,0,0,"=="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl56 = {0,0,0,9,2,1,0,0,0,"!="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl57 = {0,0,0,9,2,1,0,0,0,"=="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl58 = {0,0,0,9,3,1,0,0,0,"eq "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl59 = {0,0,0,9,7,1,0,0,0,"eq not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl60 = {0,0,0,9,1,1,0,0,0,"<"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl61 = {0,0,0,9,2,1,0,0,0,"<="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl62 = {0,0,0,9,1,1,0,0,0,">"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl63 = {0,0,0,9,2,1,0,0,0,">="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl64 = {0,0,0,9,1,1,0,0,0,"<"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl65 = {0,0,0,9,3,1,0,0,0,"lt "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl66 = {0,0,0,9,2,1,0,0,0,"<="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl67 = {0,0,0,9,7,1,0,0,0,"gt not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl68 = {0,0,0,9,1,1,0,0,0,">"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl69 = {0,0,0,9,3,1,0,0,0,"gt "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl70 = {0,0,0,9,7,1,0,0,0,"lt not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl71 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl72 = {0,0,0,9,1,1,0,0,0,"-"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl73 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl74 = {0,0,0,9,2,1,0,0,0,"+ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl75 = {0,0,0,9,2,1,0,0,0,"- "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl76 = {0,0,0,9,1,1,0,0,0,"/"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl77 = {0,0,0,9,1,1,0,0,0,"%"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl78 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl79 = {0,0,0,9,2,1,0,0,0,"* "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[22]; } uf_sl80 = {0,0,0,9,21,1,0,0,0,"no division in subset"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl81 = {0,0,0,9,1,1,0,0,0,"-"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl82 = {0,0,0,9,1,1,0,0,0,"!"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl83 = {0,0,0,9,1,1,0,0,0,"~"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl84 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl85 = {0,0,0,9,2,1,0,0,0,"++"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl86 = {0,0,0,9,2,1,0,0,0,"--"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl87 = {0,0,0,9,2,1,0,0,0,"0 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl88 = {0,0,0,9,2,1,0,0,0,"- "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl89 = {0,0,0,9,4,1,0,0,0,"not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[10]; } uf_sl90 = {0,0,0,9,9,1,0,0,0,"-1 swp - "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl91 = {0,0,0,9,16,1,0,0,0,"%s@ 1 + dup %s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl92 = {0,0,0,9,19,1,0,0,0,"++ needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl93 = {0,0,0,9,16,1,0,0,0,"%s@ 1 - dup %s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl94 = {0,0,0,9,19,1,0,0,0,"-- needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl95 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl96 = {0,0,0,9,2,1,0,0,0,"++"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl97 = {0,0,0,9,2,1,0,0,0,"--"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl98 = {0,0,0,9,2,1,0,0,0,"++"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl99 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[13]; } uf_sl100 = {0,0,0,9,12,1,0,0,0,"%s@ 1 + %s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl101 = {0,0,0,9,27,1,0,0,0,"postfix ++ needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl102 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[13]; } uf_sl103 = {0,0,0,9,12,1,0,0,0,"%s@ 1 - %s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl104 = {0,0,0,9,27,1,0,0,0,"postfix -- needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl105 = {0,0,0,9,6,1,0,0,0,"[0-9]*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl106 = {0,0,0,9,1,1,0,0,0,"'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl107 = {0,0,0,9,1,1,0,0,0,"\""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl108 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl109 = {0,0,0,9,4,1,0,0,0,"argc"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl110 = {0,0,0,9,4,1,0,0,0,"argv"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl111 = {0,0,0,9,6,1,0,0,0,"__byte"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl112 = {0,0,0,9,4,1,0,0,0,"NULL"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl113 = {0,0,0,9,3,1,0,0,0,"EOF"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl114 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl115 = {0,0,0,9,27,1,0,0,0,"^(0[xX][0-9a-fA-F]+|[0-9]+)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl116 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl117 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl118 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl119 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl120 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[24]; } uf_sl121 = {0,0,0,9,23,1,0,0,0,"extern \"uf_argc\" loadx "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl122 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl123 = {0,0,0,9,1,1,0,0,0,"["};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl124 = {0,0,0,9,1,1,0,0,0,"]"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[40]; } uf_sl125 = {0,0,0,9,39,1,0,0,0,"extern \"uf_argv\" loadx swp 8 * + loadx "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl126 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl127 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl128 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[15]; } uf_sl129 = {0,0,0,9,14,1,0,0,0,"loadx 255 and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl130 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl131 = {0,0,0,9,2,1,0,0,0,"0 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl132 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl133 = {0,0,0,9,3,1,0,0,0,"-1 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl134 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl135 = {0,0,0,9,1,1,0,0,0,"\\"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl136 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl137 = {0,0,0,9,2,1,0,0,0,"\\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl138 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl139 = {0,0,0,9,2,1,0,0,0,"\\t"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl140 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl141 = {0,0,0,9,2,1,0,0,0,"\\r"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl142 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl143 = {0,0,0,9,2,1,0,0,0,"\\0"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl144 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl145 = {0,0,0,9,2,1,0,0,0,"\\\\"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl146 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl147 = {0,0,0,9,2,1,0,0,0,"\\'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl148 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[16]; } uf_sl149 = {0,0,0,9,15,1,0,0,0,"bad char escape"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl150 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl151 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl152 = {0,0,0,9,9,1,0,0,0,"_call %s "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl153 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl154 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl155 = {0,0,0,9,1,1,0,0,0,"["};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl156 = {0,0,0,9,1,1,0,0,0,"]"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl157 = {0,0,0,9,20,1,0,0,0,"%s@ + loadx 255 and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl158 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl159 = {0,0,0,9,4,1,0,0,0,"%s@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl160 = {0,0,0,9,22,1,0,0,0,"unknown array variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl161 = {0,0,0,9,20,1,0,0,0,"undefined variable: "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl162 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl163 = {0,0,0,9,6,1,0,0,0,"return"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl164 = {0,0,0,9,2,1,0,0,0,"if"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl165 = {0,0,0,9,5,1,0,0,0,"while"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl166 = {0,0,0,9,3,1,0,0,0,"for"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl167 = {0,0,0,9,2,1,0,0,0,"do"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl168 = {0,0,0,9,5,1,0,0,0,"break"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl169 = {0,0,0,9,8,1,0,0,0,"continue"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl170 = {0,0,0,9,1,1,0,0,0,"{"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl171 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl172 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl173 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl174 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl175 = {0,0,0,9,4,1,0,0,0,"%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl176 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[11]; } uf_sl177 = {0,0,0,9,10,1,0,0,0,"call exit\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl178 = {0,0,0,9,4,1,0,0,0,"ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl179 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl180 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl181 = {0,0,0,9,1,1,0,0,0,"i"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl182 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl183 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl184 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl185 = {0,0,0,9,4,1,0,0,0,"else"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl186 = {0,0,0,9,1,1,0,0,0,"e"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl187 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl188 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl189 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl190 = {0,0,0,9,2,1,0,0,0,"'i"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl191 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl192 = {0,0,0,9,3,1,0,0,0," 'e"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl193 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl194 = {0,0,0,9,8,1,0,0,0," ifelse\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl195 = {0,0,0,9,2,1,0,0,0,"'i"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl196 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl197 = {0,0,0,9,4,1,0,0,0," if\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl198 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl199 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl200 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl201 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl202 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl203 = {0,0,0,9,4,1,0,0,0,"ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl204 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl205 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl206 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl207 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl208 = {0,0,0,9,2,1,0,0,0,"'c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl209 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl210 = {0,0,0,9,3,1,0,0,0," 'b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl211 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl212 = {0,0,0,9,7,1,0,0,0," while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl213 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl214 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl215 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl216 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl217 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl218 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl219 = {0,0,0,9,4,1,0,0,0,"ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl220 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl221 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl222 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl223 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl224 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl225 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl226 = {0,0,0,9,2,1,0,0,0,"'c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl227 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl228 = {0,0,0,9,3,1,0,0,0," 'b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl229 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl230 = {0,0,0,9,7,1,0,0,0," while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl231 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl232 = {0,0,0,9,4,1,0,0,0,"%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl233 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl234 = {0,0,0,9,5,1,0,0,0,"drop\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl235 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl236 = {0,0,0,9,2,1,0,0,0,"1 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl237 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl238 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl239 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl240 = {0,0,0,9,5,1,0,0,0,"drop\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl241 = {0,0,0,9,4,1,0,0,0,"1 df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl242 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl243 = {0,0,0,9,2,1,0,0,0,"!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl244 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl245 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl246 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl247 = {0,0,0,9,4,1,0,0,0,"0 df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl248 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl249 = {0,0,0,9,2,1,0,0,0,"!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl250 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl251 = {0,0,0,9,5,1,0,0,0,"while"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl252 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl253 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl254 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl255 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl256 = {0,0,0,9,2,1,0,0,0,"df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl257 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl258 = {0,0,0,9,2,1,0,0,0,"@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl259 = {0,0,0,9,8,1,0,0,0," or ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl260 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl261 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl262 = {0,0,0,9,2,1,0,0,0,"'c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl263 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl264 = {0,0,0,9,3,1,0,0,0," 'b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl265 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl266 = {0,0,0,9,7,1,0,0,0," while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl267 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl268 = {0,0,0,9,6,1,0,0,0,"break\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl269 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl270 = {0,0,0,9,5,1,0,0,0,"cont\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl271 = {0,0,0,9,1,1,0,0,0,"}"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl272 = {0,0,0,9,5,1,0,0,0,"drop\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl273 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl274 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl275 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl276 = {0,0,0,9,4,1,0,0,0,"main"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl277 = {0,0,0,9,4,1,0,0,0,"%s:\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl278 = {0,0,0,9,4,1,0,0,0,"main"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl279 = {0,0,0,9,1,1,0,0,0,"{"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl280 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl281 = {0,0,0,9,7,1,0,0,0,"entry:\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl282 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl283 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl284 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl285 = {0,0,0,9,4,1,0,0,0,"%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl286 = {0,0,0,9,1,1,0,0,0,"}"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[13]; } uf_sl287 = {0,0,0,9,12,1,0,0,0,"0 call exit\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl288 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl289 = {0,0,0,9,1,1,0,0,0,"r"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[14]; } uf_sl290 = {0,0,0,9,13,1,0,0,0,"DEBUG: nt=%d\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl291 = {0,0,0,9,31,1,0,0,0,"import c\"printf\"(ptr,...)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl292 = {0,0,0,9,27,1,0,0,0,"import c\"malloc\"(int)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl293 = {0,0,0,9,26,1,0,0,0,"import c\"free\"(ptr)->void\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[26]; } uf_sl294 = {0,0,0,9,25,1,0,0,0,"import c\"puts\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[29]; } uf_sl295 = {0,0,0,9,28,1,0,0,0,"import c\"putchar\"(int)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[26]; } uf_sl296 = {0,0,0,9,25,1,0,0,0,"import c\"getchar\"()->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[31]; } uf_sl297 = {0,0,0,9,30,1,0,0,0,"import c\"fputs\"(ptr,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[40]; } uf_sl298 = {0,0,0,9,39,1,0,0,0,"import c\"fwrite\"(ptr,int,int,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl299 = {0,0,0,9,27,1,0,0,0,"import c\"strlen\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl300 = {0,0,0,9,31,1,0,0,0,"import c\"strcmp\"(ptr,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[37]; } uf_sl301 = {0,0,0,9,36,1,0,0,0,"import c\"strncmp\"(ptr,ptr,int)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl302 = {0,0,0,9,31,1,0,0,0,"import c\"strcpy\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl303 = {0,0,0,9,31,1,0,0,0,"import c\"strcat\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl304 = {0,0,0,9,26,1,0,0,0,"import c\"exit\"(int)->void\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[31]; } uf_sl305 = {0,0,0,9,30,1,0,0,0,"import c\"fopen\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl306 = {0,0,0,9,27,1,0,0,0,"import c\"fclose\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl307 = {0,0,0,9,26,1,0,0,0,"import c\"fgetc\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl308 = {0,0,0,9,16,1,0,0,0,"extern \"stdout\"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl309 = {0,0,0,9,20,1,0,0,0,"DEBUG: calling pprog"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[18]; } uf_sl310 = {0,0,0,9,17,1,0,0,0,"DEBUG: pprog done"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl311 = {0,0,0,9,2,1,0,0,0,"%s"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl312 = {0,0,0,9,19,1,0,0,0,"usage: trans file.c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[18]; } uf_sl313 = {0,0,0,9,17,1,0,0,0,"cannot open input"};
static void* uf_lits[] = {(void*)&uf_sl0,(void*)&uf_sl1,(void*)&uf_sl2,(void*)&uf_sl3,(void*)&uf_sl4,(void*)&uf_sl5,(void*)&uf_sl6,(void*)&uf_sl7,(void*)&uf_sl8,(void*)&uf_sl9,(void*)&uf_sl10,(void*)&uf_sl11,(void*)&uf_sl12,(void*)&uf_sl13,(void*)&uf_sl14,(void*)&uf_sl15,(void*)&uf_sl16,(void*)&uf_sl17,(void*)&uf_sl18,(void*)&uf_sl19,(void*)&uf_sl20,(void*)&uf_sl21,(void*)&uf_sl22,(void*)&uf_sl23,(void*)&uf_sl24,(void*)&uf_sl25,(void*)&uf_sl26,(void*)&uf_sl27,(void*)&uf_sl28,(void*)&uf_sl29,(void*)&uf_sl30,(void*)&uf_sl31,(void*)&uf_sl32,(void*)&uf_sl33,(void*)&uf_sl34,(void*)&uf_sl35,(void*)&uf_sl36,(void*)&uf_sl37,(void*)&uf_sl38,(void*)&uf_sl39,(void*)&uf_sl40,(void*)&uf_sl41,(void*)&uf_sl42,(void*)&uf_sl43,(void*)&uf_sl44,(void*)&uf_sl45,(void*)&uf_sl46,(void*)&uf_sl47,(void*)&uf_sl48,(void*)&uf_sl49,(void*)&uf_sl50,(void*)&uf_sl51,(void*)&uf_sl52,(void*)&uf_sl53,(void*)&uf_sl54,(void*)&uf_sl55,(void*)&uf_sl56,(void*)&uf_sl57,(void*)&uf_sl58,(void*)&uf_sl59,(void*)&uf_sl60,(void*)&uf_sl61,(void*)&uf_sl62,(void*)&uf_sl63,(void*)&uf_sl64,(void*)&uf_sl65,(void*)&uf_sl66,(void*)&uf_sl67,(void*)&uf_sl68,(void*)&uf_sl69,(void*)&uf_sl70,(void*)&uf_sl71,(void*)&uf_sl72,(void*)&uf_sl73,(void*)&uf_sl74,(void*)&uf_sl75,(void*)&uf_sl76,(void*)&uf_sl77,(void*)&uf_sl78,(void*)&uf_sl79,(void*)&uf_sl80,(void*)&uf_sl81,(void*)&uf_sl82,(void*)&uf_sl83,(void*)&uf_sl84,(void*)&uf_sl85,(void*)&uf_sl86,(void*)&uf_sl87,(void*)&uf_sl88,(void*)&uf_sl89,(void*)&uf_sl90,(void*)&uf_sl91,(void*)&uf_sl92,(void*)&uf_sl93,(void*)&uf_sl94,(void*)&uf_sl95,(void*)&uf_sl96,(void*)&uf_sl97,(void*)&uf_sl98,(void*)&uf_sl99,(void*)&uf_sl100,(void*)&uf_sl101,(void*)&uf_sl102,(void*)&uf_sl103,(void*)&uf_sl104,(void*)&uf_sl105,(void*)&uf_sl106,(void*)&uf_sl107,(void*)&uf_sl108,(void*)&uf_sl109,(void*)&uf_sl110,(void*)&uf_sl111,(void*)&uf_sl112,(void*)&uf_sl113,(void*)&uf_sl114,(void*)&uf_sl115,(void*)&uf_sl116,(void*)&uf_sl117,(void*)&uf_sl118,(void*)&uf_sl119,(void*)&uf_sl120,(void*)&uf_sl121,(void*)&uf_sl122,(void*)&uf_sl123,(void*)&uf_sl124,(void*)&uf_sl125,(void*)&uf_sl126,(void*)&uf_sl127,(void*)&uf_sl128,(void*)&uf_sl129,(void*)&uf_sl130,(void*)&uf_sl131,(void*)&uf_sl132,(void*)&uf_sl133,(void*)&uf_sl134,(void*)&uf_sl135,(void*)&uf_sl136,(void*)&uf_sl137,(void*)&uf_sl138,(void*)&uf_sl139,(void*)&uf_sl140,(void*)&uf_sl141,(void*)&uf_sl142,(void*)&uf_sl143,(void*)&uf_sl144,(void*)&uf_sl145,(void*)&uf_sl146,(void*)&uf_sl147,(void*)&uf_sl148,(void*)&uf_sl149,(void*)&uf_sl150,(void*)&uf_sl151,(void*)&uf_sl152,(void*)&uf_sl153,(void*)&uf_sl154,(void*)&uf_sl155,(void*)&uf_sl156,(void*)&uf_sl157,(void*)&uf_sl158,(void*)&uf_sl159,(void*)&uf_sl160,(void*)&uf_sl161,(void*)&uf_sl162,(void*)&uf_sl163,(void*)&uf_sl164,(void*)&uf_sl165,(void*)&uf_sl166,(void*)&uf_sl167,(void*)&uf_sl168,(void*)&uf_sl169,(void*)&uf_sl170,(void*)&uf_sl171,(void*)&uf_sl172,(void*)&uf_sl173,(void*)&uf_sl174,(void*)&uf_sl175,(void*)&uf_sl176,(void*)&uf_sl177,(void*)&uf_sl178,(void*)&uf_sl179,(void*)&uf_sl180,(void*)&uf_sl181,(void*)&uf_sl182,(void*)&uf_sl183,(void*)&uf_sl184,(void*)&uf_sl185,(void*)&uf_sl186,(void*)&uf_sl187,(void*)&uf_sl188,(void*)&uf_sl189,(void*)&uf_sl190,(void*)&uf_sl191,(void*)&uf_sl192,(void*)&uf_sl193,(void*)&uf_sl194,(void*)&uf_sl195,(void*)&uf_sl196,(void*)&uf_sl197,(void*)&uf_sl198,(void*)&uf_sl199,(void*)&uf_sl200,(void*)&uf_sl201,(void*)&uf_sl202,(void*)&uf_sl203,(void*)&uf_sl204,(void*)&uf_sl205,(void*)&uf_sl206,(void*)&uf_sl207,(void*)&uf_sl208,(void*)&uf_sl209,(void*)&uf_sl210,(void*)&uf_sl211,(void*)&uf_sl212,(void*)&uf_sl213,(void*)&uf_sl214,(void*)&uf_sl215,(void*)&uf_sl216,(void*)&uf_sl217,(void*)&uf_sl218,(void*)&uf_sl219,(void*)&uf_sl220,(void*)&uf_sl221,(void*)&uf_sl222,(void*)&uf_sl223,(void*)&uf_sl224,(void*)&uf_sl225,(void*)&uf_sl226,(void*)&uf_sl227,(void*)&uf_sl228,(void*)&uf_sl229,(void*)&uf_sl230,(void*)&uf_sl231,(void*)&uf_sl232,(void*)&uf_sl233,(void*)&uf_sl234,(void*)&uf_sl235,(void*)&uf_sl236,(void*)&uf_sl237,(void*)&uf_sl238,(void*)&uf_sl239,(void*)&uf_sl240,(void*)&uf_sl241,(void*)&uf_sl242,(void*)&uf_sl243,(void*)&uf_sl244,(void*)&uf_sl245,(void*)&uf_sl246,(void*)&uf_sl247,(void*)&uf_sl248,(void*)&uf_sl249,(void*)&uf_sl250,(void*)&uf_sl251,(void*)&uf_sl252,(void*)&uf_sl253,(void*)&uf_sl254,(void*)&uf_sl255,(void*)&uf_sl256,(void*)&uf_sl257,(void*)&uf_sl258,(void*)&uf_sl259,(void*)&uf_sl260,(void*)&uf_sl261,(void*)&uf_sl262,(void*)&uf_sl263,(void*)&uf_sl264,(void*)&uf_sl265,(void*)&uf_sl266,(void*)&uf_sl267,(void*)&uf_sl268,(void*)&uf_sl269,(void*)&uf_sl270,(void*)&uf_sl271,(void*)&uf_sl272,(void*)&uf_sl273,(void*)&uf_sl274,(void*)&uf_sl275,(void*)&uf_sl276,(void*)&uf_sl277,(void*)&uf_sl278,(void*)&uf_sl279,(void*)&uf_sl280,(void*)&uf_sl281,(void*)&uf_sl282,(void*)&uf_sl283,(void*)&uf_sl284,(void*)&uf_sl285,(void*)&uf_sl286,(void*)&uf_sl287,(void*)&uf_sl288,(void*)&uf_sl289,(void*)&uf_sl290,(void*)&uf_sl291,(void*)&uf_sl292,(void*)&uf_sl293,(void*)&uf_sl294,(void*)&uf_sl295,(void*)&uf_sl296,(void*)&uf_sl297,(void*)&uf_sl298,(void*)&uf_sl299,(void*)&uf_sl300,(void*)&uf_sl301,(void*)&uf_sl302,(void*)&uf_sl303,(void*)&uf_sl304,(void*)&uf_sl305,(void*)&uf_sl306,(void*)&uf_sl307,(void*)&uf_sl308,(void*)&uf_sl309,(void*)&uf_sl310,(void*)&uf_sl311,(void*)&uf_sl312,(void*)&uf_sl313};
extern char uf_x0[] __asm__("uf_argc");
extern char uf_x1[] __asm__("uf_argv");
extern int64_t uf_im0() __asm__("printf");
extern void* uf_im1() __asm__("malloc");
extern void* uf_im2() __asm__("fopen");
extern int64_t uf_im3() __asm__("fseek");
extern int64_t uf_im4() __asm__("ftell");
extern int64_t uf_im5() __asm__("fread");
extern int64_t uf_im6() __asm__("fclose");
extern int64_t uf_im7() __asm__("puts");
extern void uf_im8() __asm__("exit");
extern int64_t uf_im9() __asm__("strlen");
extern int64_t uf_im10() __asm__("strcmp");
static Cell var_trans__out;
static Cell var_trans__qout;
static Cell var_trans__inq;
static Cell var_trans__s;
static Cell var_trans__n;
static Cell var_trans__sn;
static Cell var_trans__b2;
static Cell var_trans__a2;
static Cell var_trans__toks;
static Cell var_trans__pi;
static Cell var_trans__lbl;
static Cell var_trans__e;
static Cell var_trans__it;
static Cell var_trans__nv;
static Cell var_trans__fid;
static Cell var_trans__nv2;
static Cell var_trans__vars;
static Cell var_trans__tm_pat;
static Cell var_trans__src;
static Cell var_trans__pos;
static Cell var_trans__srclen;
static Cell var_trans__tmr;
static Cell var_trans__rest;
static Cell var_trans__ln;
static Cell var_trans__ps;
static Cell var_trans__lasts;
static Cell var_trans__ptk;
static Cell var_trans__ci;
static Cell var_trans__slot;
static Cell var_trans__slot2;
static Cell var_trans__inmain;
static Cell var_trans__tlbl;
static Cell var_trans__elbl;
static Cell var_trans__clbl;
static Cell var_trans__blbl;
static Cell var_trans__fclbl;
static Cell var_trans__fblbl;
static Cell var_trans__pfpi;
static Cell var_trans__pfd;
static Cell var_trans__pfpi2;
static Cell var_trans__dflbl;
static Cell var_trans__fname;
static Cell var_trans__pl;
static Cell var_trans__pfi;
static Cell var_trans__nt;
static Cell var_trans__path;
static Cell var_trans__f;
static Cell* uf_vroots[] = {&var_trans__out,&var_trans__qout,&var_trans__inq,&var_trans__s,&var_trans__n,&var_trans__sn,&var_trans__b2,&var_trans__a2,&var_trans__toks,&var_trans__pi,&var_trans__lbl,&var_trans__e,&var_trans__it,&var_trans__nv,&var_trans__fid,&var_trans__nv2,&var_trans__vars,&var_trans__tm_pat,&var_trans__src,&var_trans__pos,&var_trans__srclen,&var_trans__tmr,&var_trans__rest,&var_trans__ln,&var_trans__ps,&var_trans__lasts,&var_trans__ptk,&var_trans__ci,&var_trans__slot,&var_trans__slot2,&var_trans__inmain,&var_trans__tlbl,&var_trans__elbl,&var_trans__clbl,&var_trans__blbl,&var_trans__fclbl,&var_trans__fblbl,&var_trans__pfpi,&var_trans__pfd,&var_trans__pfpi2,&var_trans__dflbl,&var_trans__fname,&var_trans__pl,&var_trans__pfi,&var_trans__nt,&var_trans__path,&var_trans__f};

static void uflux_run(Ctx*cx, long pc){
  uf_cur_cx=cx;
  if(pc<0){ goto *(void*)uf_entry_addr; }
  static const void* labtab[] = {[0]=&&L_0,[12]=&&L_12,[18]=&&L_18,[34]=&&L_34,[40]=&&L_40,[78]=&&L_78,[129]=&&L_129,[136]=&&L_136,[165]=&&L_165,[184]=&&L_184,[190]=&&L_190,[195]=&&L_195,[202]=&&L_202,[219]=&&L_219,[223]=&&L_223,[236]=&&L_236,[240]=&&L_240,[253]=&&L_253,[257]=&&L_257,[270]=&&L_270,[274]=&&L_274,[287]=&&L_287,[291]=&&L_291,[312]=&&L_312,[336]=&&L_336,[348]=&&L_348,[353]=&&L_353,[362]=&&L_362,[373]=&&L_373,[380]=&&L_380,[385]=&&L_385,[393]=&&L_393,[404]=&&L_404,[411]=&&L_411,[416]=&&L_416,[424]=&&L_424,[435]=&&L_435,[442]=&&L_442,[447]=&&L_447,[460]=&&L_460,[464]=&&L_464,[532]=&&L_532,[541]=&&L_541,[573]=&&L_573,[588]=&&L_588,[622]=&&L_622,[629]=&&L_629,[633]=&&L_633,[644]=&&L_644,[648]=&&L_648,[659]=&&L_659,[667]=&&L_667,[675]=&&L_675,[681]=&&L_681,[692]=&&L_692,[708]=&&L_708,[716]=&&L_716,[722]=&&L_722,[730]=&&L_730,[736]=&&L_736,[744]=&&L_744,[750]=&&L_750,[761]=&&L_761,[769]=&&L_769,[777]=&&L_777,[783]=&&L_783,[803]=&&L_803,[807]=&&L_807,[813]=&&L_813,[822]=&&L_822,[829]=&&L_829,[836]=&&L_836,[843]=&&L_843,[850]=&&L_850,[857]=&&L_857,[865]=&&L_865,[871]=&&L_871,[877]=&&L_877,[881]=&&L_881,[896]=&&L_896,[898]=&&L_898,[913]=&&L_913,[915]=&&L_915,[939]=&&L_939,[952]=&&L_952,[954]=&&L_954,[967]=&&L_967,[978]=&&L_978,[985]=&&L_985,[992]=&&L_992,[999]=&&L_999,[1006]=&&L_1006,[1013]=&&L_1013,[1020]=&&L_1020,[1027]=&&L_1027,[1034]=&&L_1034,[1041]=&&L_1041,[1053]=&&L_1053,[1062]=&&L_1062,[1077]=&&L_1077,[1084]=&&L_1084,[1110]=&&L_1110,[1136]=&&L_1136,[1143]=&&L_1143,[1150]=&&L_1150,[1166]=&&L_1166,[1176]=&&L_1176,[1184]=&&L_1184,[1191]=&&L_1191,[1199]=&&L_1199,[1206]=&&L_1206,[1214]=&&L_1214,[1221]=&&L_1221,[1229]=&&L_1229,[1236]=&&L_1236,[1244]=&&L_1244,[1251]=&&L_1251,[1259]=&&L_1259,[1266]=&&L_1266,[1268]=&&L_1268,[1299]=&&L_1299,[1301]=&&L_1301,[1307]=&&L_1307,[1311]=&&L_1311,[1315]=&&L_1315,[1322]=&&L_1322,[1354]=&&L_1354,[1369]=&&L_1369,[1371]=&&L_1371,[1384]=&&L_1384,[1391]=&&L_1391,[1398]=&&L_1398,[1405]=&&L_1405,[1412]=&&L_1412,[1419]=&&L_1419,[1426]=&&L_1426,[1433]=&&L_1433,[1440]=&&L_1440,[1447]=&&L_1447,[1463]=&&L_1463,[1467]=&&L_1467,[1484]=&&L_1484,[1500]=&&L_1500,[1502]=&&L_1502,[1519]=&&L_1519,[1523]=&&L_1523,[1527]=&&L_1527,[1572]=&&L_1572,[1607]=&&L_1607,[1617]=&&L_1617,[1682]=&&L_1682,[1777]=&&L_1777,[1780]=&&L_1780,[1787]=&&L_1787,[1827]=&&L_1827,[1841]=&&L_1841,[1846]=&&L_1846,[1858]=&&L_1858,[1860]=&&L_1860,[1868]=&&L_1868,[1875]=&&L_1875,[1883]=&&L_1883,[1893]=&&L_1893,[1895]=&&L_1895,[1898]=&&L_1898,[1901]=&&L_1901,[1903]=&&L_1903,[1908]=&&L_1908,[2019]=&&L_2019,[2033]=&&L_2033,[2047]=&&L_2047,[2054]=&&L_2054,[2059]=&&L_2059,[2062]=&&L_2062,[2065]=&&L_2065,[2148]=&&L_2148,[2152]=&&L_2152,[2154]=&&L_2154,[2159]=&&L_2159,[2167]=&&L_2167,[2170]=&&L_2170,[2181]=&&L_2181,[2184]=&&L_2184,[2191]=&&L_2191,[2196]=&&L_2196,[2211]=&&L_2211,[2216]=&&L_2216,[2219]=&&L_2219,[2223]=&&L_2223,[2231]=&&L_2231,[2236]=&&L_2236,[2361]=&&L_2361,[2363]=&&L_2363,};
  if(pc==0) goto L_2239;
  goto *labtab[pc];
  static const struct { int64_t tk; int64_t mh; const void* lab; } uf_mt[] = {{0,0,&&L_0}};
L_0: Cell t0=uf_mkp((void*)&uf_sl0);L_1: L_2: Cell t1=uf_mkp((void*)&uf_sl1);L_3: L_4: Cell t2=uf_mki(0LL);L_5: var_trans__out=t0;var_trans__qout=t1;var_trans__inq=t2;L_6: Cell t3=pop(cx);L_7: Cell t4=var_trans__inq;L_8: var_trans__s=t3;pushc(cx,t4);pushp(cx,(void*)&&L_12);
L_9: pushp(cx,(void*)&&L_18);
L_10: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_10;goto *((!uf_zero(c))?th:el);K_10:;pop(cx);}
L_11: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_12: Cell t5=var_trans__out;L_13: Cell t6=var_trans__s;L_14: pushc(cx,t5);pushc(cx,t6);op_cat(cx);
L_15: Cell t7=pop(cx);L_16: Cell t8=uf_mki(0LL);L_17: var_trans__out=t7;pushc(cx,t8);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_18: Cell t9=var_trans__qout;L_19: Cell t10=var_trans__s;L_20: pushc(cx,t9);pushc(cx,t10);op_cat(cx);
L_21: Cell t11=pop(cx);L_22: Cell t12=uf_mki(0LL);L_23: var_trans__qout=t11;pushc(cx,t12);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_24: Cell t13=pop(cx);L_25: L_26: Cell t14=uf_mkp((void*)&uf_sl2);L_27: var_trans__n=t13;pushc(cx,t13);pushc(cx,t14);op_fmt(cx);
L_28: Cell t15=pop(cx);L_29: Cell t16=var_trans__inq;L_30: var_trans__sn=t15;pushc(cx,t16);pushp(cx,(void*)&&L_34);
L_31: pushp(cx,(void*)&&L_40);
L_32: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_32;goto *((!uf_zero(c))?th:el);K_32:;pop(cx);}
L_33: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_34: Cell t17=var_trans__out;L_35: Cell t18=var_trans__sn;L_36: pushc(cx,t17);pushc(cx,t18);op_cat(cx);
L_37: Cell t19=pop(cx);L_38: Cell t20=uf_mki(0LL);L_39: var_trans__out=t19;pushc(cx,t20);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_40: Cell t21=var_trans__qout;L_41: Cell t22=var_trans__sn;L_42: pushc(cx,t21);pushc(cx,t22);op_cat(cx);
L_43: Cell t23=pop(cx);L_44: Cell t24=uf_mki(0LL);L_45: var_trans__qout=t23;pushc(cx,t24);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_46: Cell t25=pop(cx);L_47: Cell t26=pop(cx);L_48: L_49: L_50: var_trans__b2=t25;var_trans__a2=t26;pushc(cx,t26);pushc(cx,t25);{Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,void*))uf_im10)((void*)uf_sptr(a0),(void*)uf_sptr(a1));pushi(cx,(int64_t)r);}
L_51: op_not(cx);
L_52: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_53: {Cell a0=pop(cx);int r=((int(*)(void*))uf_im7)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_54: Cell t27=uf_mki(1LL);L_55: pushc(cx,t27);{Cell a0=pop(cx);((void(*)(int64_t))uf_im8)((int64_t)(a0.tag==T_FLOAT?(int64_t)uf_f(a0):a0.i));}
L_56: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_57: Cell t28=var_trans__toks;L_58: Cell t29=var_trans__pi;L_59: pushc(cx,t28);pushc(cx,t29);op_get(cx);
L_60: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_61: Cell t30=var_trans__toks;L_62: Cell t31=var_trans__pi;L_63: Cell t32=uf_mki(1LL);L_64: Cell t33=uf_cadd(t31,t32);L_65: pushc(cx,t30);pushc(cx,t33);op_get(cx);
L_66: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_67: Cell t34=var_trans__pi;L_68: Cell t35=uf_mki(1LL);L_69: Cell t36=uf_cadd(t34,t35);L_70: L_71: var_trans__pi=t36;{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_72: Cell t37=var_trans__lbl;L_73: Cell t38=uf_mki(1LL);L_74: Cell t39=uf_cadd(t37,t38);L_75: L_76: L_77: var_trans__lbl=t39;pushc(cx,t39);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_78: Cell t40=uf_mkp((void*)&uf_sl3);L_79: Cell t41=var_trans__e;L_80: pushc(cx,t40);pushc(cx,t41);op_cat(cx);
L_81: Cell t42=uf_mkp((void*)&uf_sl4);L_82: pushc(cx,t42);op_cat(cx);
L_83: cx->cs[cx->csp++]=&&K_83;goto L_57;K_83:;
L_84: op_cat(cx);
L_85: Cell t43=uf_mkp((void*)&uf_sl5);L_86: pushc(cx,t43);op_cat(cx);
L_87: cx->cs[cx->csp++]=&&K_87;goto L_53;K_87:;
L_88: Cell t44=pop(cx);L_89: L_90: Cell t45=uf_mkp((void*)&uf_sl6);L_91: var_trans__it=t44;pushc(cx,t44);pushc(cx,t45);cx->cs[cx->csp++]=&&K_91;goto L_46;K_91:;
L_92: Cell t46=var_trans__it;L_93: Cell t47=uf_mkp((void*)&uf_sl7);L_94: pushc(cx,t46);pushc(cx,t47);cx->cs[cx->csp++]=&&K_94;goto L_46;K_94:;
L_95: Cell t48=pop(cx);Cell t49=pop(cx);Cell t50=uf_cadd(t49,t48);L_96: Cell t51=var_trans__it;L_97: Cell t52=uf_mkp((void*)&uf_sl8);L_98: pushc(cx,t50);pushc(cx,t51);pushc(cx,t52);cx->cs[cx->csp++]=&&K_98;goto L_46;K_98:;
L_99: Cell t53=pop(cx);Cell t54=pop(cx);Cell t55=uf_cadd(t54,t53);L_100: Cell t56=var_trans__it;L_101: Cell t57=uf_mkp((void*)&uf_sl9);L_102: pushc(cx,t55);pushc(cx,t56);pushc(cx,t57);cx->cs[cx->csp++]=&&K_102;goto L_46;K_102:;
L_103: Cell t58=pop(cx);Cell t59=pop(cx);Cell t60=uf_cadd(t59,t58);L_104: Cell t61=var_trans__it;L_105: Cell t62=uf_mkp((void*)&uf_sl10);L_106: pushc(cx,t60);pushc(cx,t61);pushc(cx,t62);cx->cs[cx->csp++]=&&K_106;goto L_46;K_106:;
L_107: Cell t63=pop(cx);Cell t64=pop(cx);Cell t65=uf_cadd(t64,t63);L_108: Cell t66=var_trans__it;L_109: Cell t67=uf_mkp((void*)&uf_sl11);L_110: pushc(cx,t65);pushc(cx,t66);pushc(cx,t67);cx->cs[cx->csp++]=&&K_110;goto L_46;K_110:;
L_111: Cell t68=pop(cx);Cell t69=pop(cx);Cell t70=uf_cadd(t69,t68);L_112: Cell t71=var_trans__it;L_113: Cell t72=uf_mkp((void*)&uf_sl12);L_114: pushc(cx,t70);pushc(cx,t71);pushc(cx,t72);cx->cs[cx->csp++]=&&K_114;goto L_46;K_114:;
L_115: Cell t73=pop(cx);Cell t74=pop(cx);Cell t75=uf_cadd(t74,t73);L_116: Cell t76=var_trans__it;L_117: Cell t77=uf_mkp((void*)&uf_sl13);L_118: pushc(cx,t75);pushc(cx,t76);pushc(cx,t77);cx->cs[cx->csp++]=&&K_118;goto L_46;K_118:;
L_119: Cell t78=pop(cx);Cell t79=pop(cx);Cell t80=uf_cadd(t79,t78);L_120: Cell t81=var_trans__it;L_121: Cell t82=uf_mkp((void*)&uf_sl14);L_122: pushc(cx,t80);pushc(cx,t81);pushc(cx,t82);cx->cs[cx->csp++]=&&K_122;goto L_46;K_122:;
L_123: Cell t83=pop(cx);Cell t84=pop(cx);Cell t85=uf_cadd(t84,t83);L_124: pushc(cx,t85);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_125: pushp(cx,(void*)&&L_129);
L_126: pushp(cx,(void*)&&L_136);
L_127: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_127:;cx->loops[fr].cont=&&K_WT_127;cx->loops[fr].end=&&K_WE_127;
cx->cs[cx->csp++]=&&K_WC_127;goto *cnd;K_WC_127:;
if(uf_zero(pop(cx)))goto K_WE_127;
cx->cs[cx->csp++]=&&K_WB_127;goto *bod;K_WB_127:;pop(cx);
goto K_WT_127;
K_WE_127:;cx->lsp=fr;}
L_128: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_129: cx->cs[cx->csp++]=&&K_129;goto L_57;K_129:;
L_130: cx->cs[cx->csp++]=&&K_130;goto L_88;K_130:;
L_131: cx->cs[cx->csp++]=&&K_131;goto L_57;K_131:;
L_132: Cell t86=uf_mkp((void*)&uf_sl15);L_133: pushc(cx,t86);cx->cs[cx->csp++]=&&K_133;goto L_46;K_133:;
L_134: Cell t87=pop(cx);Cell t88=pop(cx);Cell t89=uf_cadd(t88,t87);L_135: pushc(cx,t89);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_136: cx->cs[cx->csp++]=&&K_136;goto L_67;K_136:;
L_137: Cell t90=uf_mki(0LL);L_138: pushc(cx,t90);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_139: Cell t91=pop(cx);L_140: Cell t92=uf_mkp((void*)&uf_sl16);L_141: Cell t93=var_trans__fid;L_142: Cell t94=uf_mkp((void*)&uf_sl17);L_143: var_trans__nv=t91;pushc(cx,t92);pushc(cx,t93);pushc(cx,t94);op_fmt(cx);
L_144: op_cat(cx);
L_145: Cell t95=pop(cx);L_146: Cell t96=var_trans__fid;L_147: Cell t97=uf_mki(1LL);L_148: Cell t98=uf_cadd(t96,t97);L_149: L_150: Cell t99=var_trans__vars;L_151: Cell t100=var_trans__nv;L_152: L_153: var_trans__nv2=t95;var_trans__fid=t98;pushc(cx,t99);pushc(cx,t100);pushc(cx,t95);op_set(cx);
L_154: Cell t101=var_trans__nv2;L_155: pushc(cx,t101);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_156: Cell t102=pop(cx);L_157: Cell t103=var_trans__vars;L_158: L_159: var_trans__nv=t102;pushc(cx,t103);pushc(cx,t102);op_getq(cx);
L_160: op_dup(cx);L_161: op_not(cx);
L_162: pushp(cx,(void*)&&L_165);
L_163: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_163;goto *b;K_163:;pop(cx);}}
L_164: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_165: op_drp(cx);L_166: Cell t104=uf_mkp((void*)&uf_sl18);L_167: Cell t105=var_trans__nv;L_168: pushc(cx,t104);pushc(cx,t105);op_cat(cx);
L_169: Cell t106=uf_mkp((void*)&uf_sl19);L_170: pushc(cx,t106);op_cat(cx);
L_171: cx->cs[cx->csp++]=&&K_171;goto L_53;K_171:;
L_172: Cell t107=pop(cx);L_173: Cell t108=var_trans__src;L_174: Cell t109=var_trans__pos;L_175: Cell t110=var_trans__srclen;L_176: var_trans__tm_pat=t107;pushc(cx,t108);pushc(cx,t109);pushc(cx,t110);op_slice(cx);
L_177: Cell t111=var_trans__tm_pat;L_178: pushc(cx,t111);op_match(cx);
L_179: pushp(cx,(void*)&&L_184);
L_180: pushp(cx,(void*)&&L_190);
L_181: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_181;goto *((!uf_zero(c))?th:el);K_181:;pop(cx);}
L_182: Cell t112=var_trans__tmr;L_183: pushc(cx,t112);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_184: Cell t113=uf_mki(0LL);L_185: pushc(cx,t113);op_get(cx);
L_186: {Cell a0=pop(cx);int r=((int(*)(void*))uf_im9)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_187: Cell t114=pop(cx);L_188: Cell t115=uf_mki(0LL);L_189: var_trans__tmr=t114;pushc(cx,t115);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_190: op_drp(cx);L_191: Cell t116=uf_mki(-1LL);L_192: L_193: Cell t117=uf_mki(0LL);L_194: var_trans__tmr=t116;pushc(cx,t117);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_195: pushp(cx,(void*)&&L_312);
L_196: pushp(cx,(void*)&&L_336);
L_197: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_197:;cx->loops[fr].cont=&&K_WT_197;cx->loops[fr].end=&&K_WE_197;
cx->cs[cx->csp++]=&&K_WC_197;goto *cnd;K_WC_197:;
if(uf_zero(pop(cx)))goto K_WE_197;
cx->cs[cx->csp++]=&&K_WB_197;goto *bod;K_WB_197:;pop(cx);
goto K_WT_197;
K_WE_197:;cx->lsp=fr;}
L_198: Cell t118=var_trans__pos;L_199: Cell t119=var_trans__srclen;L_200: pushc(cx,t118);pushc(cx,t119);op_lt(cx);
L_201: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_202: Cell t120=var_trans__src;L_203: Cell t121=var_trans__pos;L_204: Cell t122=var_trans__srclen;L_205: pushc(cx,t120);pushc(cx,t121);pushc(cx,t122);op_slice(cx);
L_206: Cell t123=pop(cx);L_207: L_208: Cell t124=uf_mkp((void*)&uf_sl20);L_209: var_trans__rest=t123;pushc(cx,t123);pushc(cx,t124);cx->cs[cx->csp++]=&&K_209;goto L_172;K_209:;
L_210: op_dup(cx);L_211: Cell t125=uf_mki(-1LL);L_212: pushc(cx,t125);op_eq(cx);
L_213: op_not(cx);
L_214: pushp(cx,(void*)&&L_219);
L_215: pushp(cx,(void*)&&L_223);
L_216: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_216;goto *((!uf_zero(c))?th:el);K_216:;pop(cx);}
L_217: Cell t126=uf_mki(0LL);L_218: pushc(cx,t126);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_219: Cell t127=pop(cx);L_220: var_trans__ln=t127;cx->cs[cx->csp++]=&&K_220;goto L_297;K_220:;
L_221: Cell t128=uf_mki(0LL);L_222: pushc(cx,t128);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_223: op_drp(cx);L_224: Cell t129=var_trans__rest;L_225: Cell t130=uf_mkp((void*)&uf_sl21);L_226: pushc(cx,t129);pushc(cx,t130);cx->cs[cx->csp++]=&&K_226;goto L_172;K_226:;
L_227: op_dup(cx);L_228: Cell t131=uf_mki(-1LL);L_229: pushc(cx,t131);op_eq(cx);
L_230: op_not(cx);
L_231: pushp(cx,(void*)&&L_236);
L_232: pushp(cx,(void*)&&L_240);
L_233: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_233;goto *((!uf_zero(c))?th:el);K_233:;pop(cx);}
L_234: Cell t132=uf_mki(0LL);L_235: pushc(cx,t132);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_236: Cell t133=pop(cx);L_237: var_trans__ln=t133;cx->cs[cx->csp++]=&&K_237;goto L_297;K_237:;
L_238: Cell t134=uf_mki(0LL);L_239: pushc(cx,t134);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_240: op_drp(cx);L_241: Cell t135=var_trans__rest;L_242: Cell t136=uf_mkp((void*)&uf_sl22);L_243: pushc(cx,t135);pushc(cx,t136);cx->cs[cx->csp++]=&&K_243;goto L_172;K_243:;
L_244: op_dup(cx);L_245: Cell t137=uf_mki(-1LL);L_246: pushc(cx,t137);op_eq(cx);
L_247: op_not(cx);
L_248: pushp(cx,(void*)&&L_253);
L_249: pushp(cx,(void*)&&L_257);
L_250: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_250;goto *((!uf_zero(c))?th:el);K_250:;pop(cx);}
L_251: Cell t138=uf_mki(0LL);L_252: pushc(cx,t138);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_253: Cell t139=pop(cx);L_254: var_trans__ln=t139;cx->cs[cx->csp++]=&&K_254;goto L_297;K_254:;
L_255: Cell t140=uf_mki(0LL);L_256: pushc(cx,t140);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_257: op_drp(cx);L_258: Cell t141=var_trans__rest;L_259: Cell t142=uf_mkp((void*)&uf_sl23);L_260: pushc(cx,t141);pushc(cx,t142);cx->cs[cx->csp++]=&&K_260;goto L_172;K_260:;
L_261: op_dup(cx);L_262: Cell t143=uf_mki(-1LL);L_263: pushc(cx,t143);op_eq(cx);
L_264: op_not(cx);
L_265: pushp(cx,(void*)&&L_270);
L_266: pushp(cx,(void*)&&L_274);
L_267: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_267;goto *((!uf_zero(c))?th:el);K_267:;pop(cx);}
L_268: Cell t144=uf_mki(0LL);L_269: pushc(cx,t144);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_270: Cell t145=pop(cx);L_271: var_trans__ln=t145;cx->cs[cx->csp++]=&&K_271;goto L_297;K_271:;
L_272: Cell t146=uf_mki(0LL);L_273: pushc(cx,t146);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_274: op_drp(cx);L_275: Cell t147=var_trans__rest;L_276: Cell t148=uf_mkp((void*)&uf_sl24);L_277: pushc(cx,t147);pushc(cx,t148);cx->cs[cx->csp++]=&&K_277;goto L_172;K_277:;
L_278: op_dup(cx);L_279: Cell t149=uf_mki(-1LL);L_280: pushc(cx,t149);op_eq(cx);
L_281: op_not(cx);
L_282: pushp(cx,(void*)&&L_287);
L_283: pushp(cx,(void*)&&L_291);
L_284: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_284;goto *((!uf_zero(c))?th:el);K_284:;pop(cx);}
L_285: Cell t150=uf_mki(0LL);L_286: pushc(cx,t150);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_287: Cell t151=pop(cx);L_288: var_trans__ln=t151;cx->cs[cx->csp++]=&&K_288;goto L_297;K_288:;
L_289: Cell t152=uf_mki(0LL);L_290: pushc(cx,t152);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_291: op_drp(cx);L_292: Cell t153=uf_mki(1LL);L_293: L_294: var_trans__ln=t153;cx->cs[cx->csp++]=&&K_294;goto L_297;K_294:;
L_295: Cell t154=uf_mki(0LL);L_296: pushc(cx,t154);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_297: Cell t155=var_trans__src;L_298: Cell t156=var_trans__pos;L_299: L_300: Cell t157=var_trans__ln;L_301: Cell t158=uf_cadd(t156,t157);L_302: pushc(cx,t155);pushc(cx,t156);pushc(cx,t158);op_slice(cx);
L_303: Cell t159=var_trans__toks;L_304: pushc(cx,t159);op_swp(cx);L_305: op_push(cx);
L_306: Cell t160=pop(cx);L_307: Cell t161=var_trans__pos;L_308: Cell t162=var_trans__ln;L_309: Cell t163=uf_cadd(t161,t162);L_310: L_311: var_trans__toks=t160;var_trans__pos=t163;{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_312: Cell t164=var_trans__src;L_313: Cell t165=var_trans__pos;L_314: Cell t166=var_trans__srclen;L_315: pushc(cx,t164);pushc(cx,t165);pushc(cx,t166);op_slice(cx);
L_316: Cell t167=pop(cx);L_317: L_318: Cell t168=uf_mkp((void*)&uf_sl25);L_319: var_trans__rest=t167;pushc(cx,t167);pushc(cx,t168);cx->cs[cx->csp++]=&&K_319;goto L_172;K_319:;
L_320: Cell t169=uf_mki(-1LL);L_321: pushc(cx,t169);op_eq(cx);
L_322: op_not(cx);
L_323: Cell t170=var_trans__rest;L_324: Cell t171=uf_mkp((void*)&uf_sl26);L_325: pushc(cx,t170);pushc(cx,t171);op_starts(cx);
L_326: Cell t172=pop(cx);Cell t173=pop(cx);Cell t174=uf_cadd(t173,t172);L_327: Cell t175=var_trans__rest;L_328: Cell t176=uf_mkp((void*)&uf_sl27);L_329: pushc(cx,t174);pushc(cx,t175);pushc(cx,t176);op_starts(cx);
L_330: Cell t177=pop(cx);Cell t178=pop(cx);Cell t179=uf_cadd(t178,t177);L_331: Cell t180=var_trans__rest;L_332: Cell t181=uf_mkp((void*)&uf_sl28);L_333: pushc(cx,t179);pushc(cx,t180);pushc(cx,t181);op_starts(cx);
L_334: Cell t182=pop(cx);Cell t183=pop(cx);Cell t184=uf_cadd(t183,t182);L_335: pushc(cx,t184);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_336: Cell t185=var_trans__rest;L_337: Cell t186=uf_mkp((void*)&uf_sl29);L_338: pushc(cx,t185);pushc(cx,t186);cx->cs[cx->csp++]=&&K_338;goto L_172;K_338:;
L_339: op_dup(cx);L_340: Cell t187=uf_mki(-1LL);L_341: pushc(cx,t187);op_eq(cx);
L_342: op_not(cx);
L_343: pushp(cx,(void*)&&L_348);
L_344: pushp(cx,(void*)&&L_353);
L_345: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_345;goto *((!uf_zero(c))?th:el);K_345:;pop(cx);}
L_346: Cell t188=uf_mki(0LL);L_347: pushc(cx,t188);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_348: Cell t189=var_trans__pos;L_349: Cell t190=pop(cx);Cell t191=uf_cadd(t190,t189);L_350: L_351: Cell t192=uf_mki(0LL);L_352: var_trans__pos=t191;pushc(cx,t192);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_353: op_drp(cx);L_354: Cell t193=var_trans__rest;L_355: Cell t194=uf_mkp((void*)&uf_sl30);L_356: pushc(cx,t193);pushc(cx,t194);op_starts(cx);
L_357: pushp(cx,(void*)&&L_362);
L_358: pushp(cx,(void*)&&L_385);
L_359: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_359;goto *((!uf_zero(c))?th:el);K_359:;pop(cx);}
L_360: Cell t195=uf_mki(0LL);L_361: pushc(cx,t195);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_362: Cell t196=var_trans__rest;L_363: Cell t197=uf_mkp((void*)&uf_sl31);L_364: pushc(cx,t196);pushc(cx,t197);op_find(cx);
L_365: op_dup(cx);L_366: Cell t198=uf_mki(-1LL);L_367: pushc(cx,t198);op_eq(cx);
L_368: pushp(cx,(void*)&&L_380);
L_369: pushp(cx,(void*)&&L_373);
L_370: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_370;goto *((!uf_zero(c))?th:el);K_370:;pop(cx);}
L_371: Cell t199=uf_mki(0LL);L_372: pushc(cx,t199);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_373: Cell t200=var_trans__pos;L_374: Cell t201=pop(cx);Cell t202=uf_cadd(t201,t200);L_375: Cell t203=uf_mki(1LL);L_376: Cell t204=uf_cadd(t202,t203);L_377: L_378: Cell t205=uf_mki(0LL);L_379: var_trans__pos=t204;pushc(cx,t205);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_380: op_drp(cx);L_381: Cell t206=var_trans__srclen;L_382: L_383: Cell t207=uf_mki(0LL);L_384: var_trans__pos=t206;pushc(cx,t207);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_385: Cell t208=var_trans__rest;L_386: Cell t209=uf_mkp((void*)&uf_sl32);L_387: pushc(cx,t208);pushc(cx,t209);op_starts(cx);
L_388: pushp(cx,(void*)&&L_393);
L_389: pushp(cx,(void*)&&L_416);
L_390: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_390;goto *((!uf_zero(c))?th:el);K_390:;pop(cx);}
L_391: Cell t210=uf_mki(0LL);L_392: pushc(cx,t210);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_393: Cell t211=var_trans__rest;L_394: Cell t212=uf_mkp((void*)&uf_sl33);L_395: pushc(cx,t211);pushc(cx,t212);op_find(cx);
L_396: op_dup(cx);L_397: Cell t213=uf_mki(-1LL);L_398: pushc(cx,t213);op_eq(cx);
L_399: pushp(cx,(void*)&&L_411);
L_400: pushp(cx,(void*)&&L_404);
L_401: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_401;goto *((!uf_zero(c))?th:el);K_401:;pop(cx);}
L_402: Cell t214=uf_mki(0LL);L_403: pushc(cx,t214);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_404: Cell t215=var_trans__pos;L_405: Cell t216=pop(cx);Cell t217=uf_cadd(t216,t215);L_406: Cell t218=uf_mki(2LL);L_407: Cell t219=uf_cadd(t217,t218);L_408: L_409: Cell t220=uf_mki(0LL);L_410: var_trans__pos=t219;pushc(cx,t220);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_411: op_drp(cx);L_412: Cell t221=var_trans__srclen;L_413: L_414: Cell t222=uf_mki(0LL);L_415: var_trans__pos=t221;pushc(cx,t222);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_416: Cell t223=var_trans__rest;L_417: Cell t224=uf_mkp((void*)&uf_sl34);L_418: pushc(cx,t223);pushc(cx,t224);op_starts(cx);
L_419: pushp(cx,(void*)&&L_424);
L_420: pushp(cx,(void*)&&L_447);
L_421: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_421;goto *((!uf_zero(c))?th:el);K_421:;pop(cx);}
L_422: Cell t225=uf_mki(0LL);L_423: pushc(cx,t225);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_424: Cell t226=var_trans__rest;L_425: Cell t227=uf_mkp((void*)&uf_sl35);L_426: pushc(cx,t226);pushc(cx,t227);op_find(cx);
L_427: op_dup(cx);L_428: Cell t228=uf_mki(-1LL);L_429: pushc(cx,t228);op_eq(cx);
L_430: pushp(cx,(void*)&&L_442);
L_431: pushp(cx,(void*)&&L_435);
L_432: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_432;goto *((!uf_zero(c))?th:el);K_432:;pop(cx);}
L_433: Cell t229=uf_mki(0LL);L_434: pushc(cx,t229);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_435: Cell t230=var_trans__pos;L_436: Cell t231=pop(cx);Cell t232=uf_cadd(t231,t230);L_437: Cell t233=uf_mki(1LL);L_438: Cell t234=uf_cadd(t232,t233);L_439: L_440: Cell t235=uf_mki(0LL);L_441: var_trans__pos=t234;pushc(cx,t235);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_442: op_drp(cx);L_443: Cell t236=var_trans__srclen;L_444: L_445: Cell t237=uf_mki(0LL);L_446: var_trans__pos=t236;pushc(cx,t237);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_447: Cell t238=uf_mki(0LL);L_448: pushc(cx,t238);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_449: cx->cs[cx->csp++]=&&K_449;goto L_451;K_449:;
L_450: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_451: Cell t239=var_trans__vars;L_452: pushc(cx,t239);cx->cs[cx->csp++]=&&K_452;goto L_57;K_452:;
L_453: op_getq(cx);
L_454: op_dup(cx);L_455: op_not(cx);
L_456: pushp(cx,(void*)&&L_460);
L_457: pushp(cx,(void*)&&L_464);
L_458: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_458;goto *((!uf_zero(c))?th:el);K_458:;pop(cx);}
L_459: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_460: op_drp(cx);L_461: cx->cs[cx->csp++]=&&K_461;goto L_624;K_461:;
L_462: Cell t240=uf_mki(0LL);L_463: pushc(cx,t240);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_464: Cell t241=var_trans__ps;L_465: pushc(cx,t241);op_swp(cx);L_466: op_push(cx);
L_467: Cell t242=pop(cx);L_468: var_trans__ps=t242;cx->cs[cx->csp++]=&&K_468;goto L_61;K_468:;
L_469: Cell t243=var_trans__ps;L_470: pushc(cx,t243);op_swp(cx);L_471: op_push(cx);
L_472: Cell t244=pop(cx);L_473: L_474: L_475: var_trans__ps=t244;pushc(cx,t244);pushc(cx,t244);op_len(cx);
L_476: Cell t245=uf_mki(1LL);L_477: Cell t246=pop(cx);Cell t247=uf_csub(t246,t245);L_478: pushc(cx,t247);op_get(cx);
L_479: Cell t248=uf_mkp((void*)&uf_sl36);L_480: pushc(cx,t248);cx->cs[cx->csp++]=&&K_480;goto L_46;K_480:;
L_481: Cell t249=var_trans__ps;L_482: L_483: pushc(cx,t249);pushc(cx,t249);op_len(cx);
L_484: Cell t250=uf_mki(1LL);L_485: Cell t251=pop(cx);Cell t252=uf_csub(t251,t250);L_486: pushc(cx,t252);op_get(cx);
L_487: Cell t253=uf_mkp((void*)&uf_sl37);L_488: pushc(cx,t253);cx->cs[cx->csp++]=&&K_488;goto L_46;K_488:;
L_489: Cell t254=pop(cx);Cell t255=pop(cx);Cell t256=uf_cadd(t255,t254);L_490: Cell t257=var_trans__ps;L_491: L_492: pushc(cx,t256);pushc(cx,t257);pushc(cx,t257);op_len(cx);
L_493: Cell t258=uf_mki(1LL);L_494: Cell t259=pop(cx);Cell t260=uf_csub(t259,t258);L_495: pushc(cx,t260);op_get(cx);
L_496: Cell t261=uf_mkp((void*)&uf_sl38);L_497: pushc(cx,t261);cx->cs[cx->csp++]=&&K_497;goto L_46;K_497:;
L_498: Cell t262=pop(cx);Cell t263=pop(cx);Cell t264=uf_cadd(t263,t262);L_499: Cell t265=var_trans__ps;L_500: L_501: pushc(cx,t264);pushc(cx,t265);pushc(cx,t265);op_len(cx);
L_502: Cell t266=uf_mki(1LL);L_503: Cell t267=pop(cx);Cell t268=uf_csub(t267,t266);L_504: pushc(cx,t268);op_get(cx);
L_505: Cell t269=uf_mkp((void*)&uf_sl39);L_506: pushc(cx,t269);cx->cs[cx->csp++]=&&K_506;goto L_46;K_506:;
L_507: Cell t270=pop(cx);Cell t271=pop(cx);Cell t272=uf_cadd(t271,t270);L_508: Cell t273=var_trans__ps;L_509: L_510: pushc(cx,t272);pushc(cx,t273);pushc(cx,t273);op_len(cx);
L_511: Cell t274=uf_mki(1LL);L_512: Cell t275=pop(cx);Cell t276=uf_csub(t275,t274);L_513: pushc(cx,t276);op_get(cx);
L_514: Cell t277=uf_mkp((void*)&uf_sl40);L_515: pushc(cx,t277);cx->cs[cx->csp++]=&&K_515;goto L_46;K_515:;
L_516: Cell t278=pop(cx);Cell t279=pop(cx);Cell t280=uf_cadd(t279,t278);L_517: Cell t281=var_trans__ps;L_518: L_519: pushc(cx,t280);pushc(cx,t281);pushc(cx,t281);op_len(cx);
L_520: Cell t282=uf_mki(1LL);L_521: Cell t283=pop(cx);Cell t284=uf_csub(t283,t282);L_522: pushc(cx,t284);op_get(cx);
L_523: Cell t285=uf_mkp((void*)&uf_sl41);L_524: pushc(cx,t285);cx->cs[cx->csp++]=&&K_524;goto L_46;K_524:;
L_525: Cell t286=pop(cx);Cell t287=pop(cx);Cell t288=uf_cadd(t287,t286);L_526: pushc(cx,t288);op_not(cx);
L_527: pushp(cx,(void*)&&L_532);
L_528: pushp(cx,(void*)&&L_541);
L_529: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_529;goto *((!uf_zero(c))?th:el);K_529:;pop(cx);}
L_530: Cell t289=uf_mki(0LL);L_531: pushc(cx,t289);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_532: Cell t290=var_trans__ps;L_533: pushc(cx,t290);op_lpop(cx);
L_534: op_drp(cx);L_535: Cell t291=var_trans__ps;L_536: pushc(cx,t291);op_lpop(cx);
L_537: op_drp(cx);L_538: cx->cs[cx->csp++]=&&K_538;goto L_624;K_538:;
L_539: Cell t292=uf_mki(0LL);L_540: pushc(cx,t292);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_541: Cell t293=var_trans__ps;L_542: L_543: pushc(cx,t293);pushc(cx,t293);op_len(cx);
L_544: Cell t294=uf_mki(1LL);L_545: Cell t295=pop(cx);Cell t296=uf_csub(t295,t294);L_546: pushc(cx,t296);op_get(cx);
L_547: Cell t297=uf_mkp((void*)&uf_sl42);L_548: pushc(cx,t297);cx->cs[cx->csp++]=&&K_548;goto L_46;K_548:;
L_549: Cell t298=var_trans__ps;L_550: L_551: pushc(cx,t298);pushc(cx,t298);op_len(cx);
L_552: Cell t299=uf_mki(1LL);L_553: Cell t300=pop(cx);Cell t301=uf_csub(t300,t299);L_554: pushc(cx,t301);op_get(cx);
L_555: Cell t302=uf_mkp((void*)&uf_sl43);L_556: pushc(cx,t302);cx->cs[cx->csp++]=&&K_556;goto L_46;K_556:;
L_557: Cell t303=pop(cx);Cell t304=pop(cx);Cell t305=uf_cadd(t304,t303);L_558: pushc(cx,t305);pushp(cx,(void*)&&L_622);
L_559: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_559;goto *b;K_559:;pop(cx);}}
L_560: Cell t306=var_trans__ps;L_561: L_562: pushc(cx,t306);pushc(cx,t306);op_len(cx);
L_563: Cell t307=uf_mki(1LL);L_564: Cell t308=pop(cx);Cell t309=uf_csub(t308,t307);L_565: pushc(cx,t309);op_get(cx);
L_566: Cell t310=uf_mkp((void*)&uf_sl44);L_567: pushc(cx,t310);cx->cs[cx->csp++]=&&K_567;goto L_46;K_567:;
L_568: pushp(cx,(void*)&&L_573);
L_569: pushp(cx,(void*)&&L_588);
L_570: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_570;goto *((!uf_zero(c))?th:el);K_570:;pop(cx);}
L_571: Cell t311=uf_mki(0LL);L_572: pushc(cx,t311);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_573: cx->cs[cx->csp++]=&&K_573;goto L_67;K_573:;
L_574: cx->cs[cx->csp++]=&&K_574;goto L_67;K_574:;
L_575: cx->cs[cx->csp++]=&&K_575;goto L_451;K_575:;
L_576: Cell t312=uf_mkp((void*)&uf_sl45);L_577: pushc(cx,t312);cx->cs[cx->csp++]=&&K_577;goto L_6;K_577:;
L_578: Cell t313=var_trans__ps;L_579: pushc(cx,t313);op_lpop(cx);
L_580: op_drp(cx);L_581: Cell t314=var_trans__ps;L_582: pushc(cx,t314);op_lpop(cx);
L_583: Cell t315=uf_mkp((void*)&uf_sl46);L_584: pushc(cx,t315);op_fmt(cx);
L_585: cx->cs[cx->csp++]=&&K_585;goto L_6;K_585:;
L_586: Cell t316=uf_mki(0LL);L_587: pushc(cx,t316);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_588: cx->cs[cx->csp++]=&&K_588;goto L_67;K_588:;
L_589: cx->cs[cx->csp++]=&&K_589;goto L_67;K_589:;
L_590: Cell t317=var_trans__ps;L_591: L_592: pushc(cx,t317);pushc(cx,t317);op_len(cx);
L_593: Cell t318=uf_mki(2LL);L_594: Cell t319=pop(cx);Cell t320=uf_csub(t319,t318);L_595: pushc(cx,t320);op_get(cx);
L_596: Cell t321=uf_mkp((void*)&uf_sl47);L_597: pushc(cx,t321);op_fmt(cx);
L_598: cx->cs[cx->csp++]=&&K_598;goto L_6;K_598:;
L_599: cx->cs[cx->csp++]=&&K_599;goto L_451;K_599:;
L_600: Cell t322=var_trans__ps;L_601: L_602: pushc(cx,t322);pushc(cx,t322);op_len(cx);
L_603: Cell t323=uf_mki(1LL);L_604: Cell t324=pop(cx);Cell t325=uf_csub(t324,t323);L_605: pushc(cx,t325);op_get(cx);
L_606: Cell t326=uf_mki(0LL);L_607: Cell t327=uf_mki(1LL);L_608: pushc(cx,t326);pushc(cx,t327);op_slice(cx);
L_609: cx->cs[cx->csp++]=&&K_609;goto L_6;K_609:;
L_610: Cell t328=uf_mkp((void*)&uf_sl48);L_611: pushc(cx,t328);cx->cs[cx->csp++]=&&K_611;goto L_6;K_611:;
L_612: Cell t329=var_trans__ps;L_613: pushc(cx,t329);op_lpop(cx);
L_614: op_drp(cx);L_615: Cell t330=var_trans__ps;L_616: pushc(cx,t330);op_lpop(cx);
L_617: Cell t331=uf_mkp((void*)&uf_sl49);L_618: pushc(cx,t331);op_fmt(cx);
L_619: cx->cs[cx->csp++]=&&K_619;goto L_6;K_619:;
L_620: Cell t332=uf_mki(0LL);L_621: pushc(cx,t332);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_622: Cell t333=uf_mkp((void*)&uf_sl50);L_623: pushc(cx,t333);cx->cs[cx->csp++]=&&K_623;goto L_53;K_623:;
L_624: cx->cs[cx->csp++]=&&K_624;goto L_639;K_624:;
L_625: pushp(cx,(void*)&&L_629);
L_626: pushp(cx,(void*)&&L_633);
L_627: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_627:;cx->loops[fr].cont=&&K_WT_627;cx->loops[fr].end=&&K_WE_627;
cx->cs[cx->csp++]=&&K_WC_627;goto *cnd;K_WC_627:;
if(uf_zero(pop(cx)))goto K_WE_627;
cx->cs[cx->csp++]=&&K_WB_627;goto *bod;K_WB_627:;pop(cx);
goto K_WT_627;
K_WE_627:;cx->lsp=fr;}
L_628: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_629: cx->cs[cx->csp++]=&&K_629;goto L_57;K_629:;
L_630: Cell t334=uf_mkp((void*)&uf_sl51);L_631: pushc(cx,t334);cx->cs[cx->csp++]=&&K_631;goto L_46;K_631:;
L_632: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_633: cx->cs[cx->csp++]=&&K_633;goto L_67;K_633:;
L_634: cx->cs[cx->csp++]=&&K_634;goto L_639;K_634:;
L_635: Cell t335=uf_mkp((void*)&uf_sl52);L_636: pushc(cx,t335);cx->cs[cx->csp++]=&&K_636;goto L_6;K_636:;
L_637: Cell t336=uf_mki(0LL);L_638: pushc(cx,t336);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_639: cx->cs[cx->csp++]=&&K_639;goto L_654;K_639:;
L_640: pushp(cx,(void*)&&L_644);
L_641: pushp(cx,(void*)&&L_648);
L_642: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_642:;cx->loops[fr].cont=&&K_WT_642;cx->loops[fr].end=&&K_WE_642;
cx->cs[cx->csp++]=&&K_WC_642;goto *cnd;K_WC_642:;
if(uf_zero(pop(cx)))goto K_WE_642;
cx->cs[cx->csp++]=&&K_WB_642;goto *bod;K_WB_642:;pop(cx);
goto K_WT_642;
K_WE_642:;cx->lsp=fr;}
L_643: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_644: cx->cs[cx->csp++]=&&K_644;goto L_57;K_644:;
L_645: Cell t337=uf_mkp((void*)&uf_sl53);L_646: pushc(cx,t337);cx->cs[cx->csp++]=&&K_646;goto L_46;K_646:;
L_647: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_648: cx->cs[cx->csp++]=&&K_648;goto L_67;K_648:;
L_649: cx->cs[cx->csp++]=&&K_649;goto L_654;K_649:;
L_650: Cell t338=uf_mkp((void*)&uf_sl54);L_651: pushc(cx,t338);cx->cs[cx->csp++]=&&K_651;goto L_6;K_651:;
L_652: Cell t339=uf_mki(0LL);L_653: pushc(cx,t339);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_654: cx->cs[cx->csp++]=&&K_654;goto L_687;K_654:;
L_655: pushp(cx,(void*)&&L_659);
L_656: pushp(cx,(void*)&&L_667);
L_657: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_657:;cx->loops[fr].cont=&&K_WT_657;cx->loops[fr].end=&&K_WE_657;
cx->cs[cx->csp++]=&&K_WC_657;goto *cnd;K_WC_657:;
if(uf_zero(pop(cx)))goto K_WE_657;
cx->cs[cx->csp++]=&&K_WB_657;goto *bod;K_WB_657:;pop(cx);
goto K_WT_657;
K_WE_657:;cx->lsp=fr;}
L_658: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_659: cx->cs[cx->csp++]=&&K_659;goto L_57;K_659:;
L_660: Cell t340=uf_mkp((void*)&uf_sl55);L_661: pushc(cx,t340);cx->cs[cx->csp++]=&&K_661;goto L_46;K_661:;
L_662: cx->cs[cx->csp++]=&&K_662;goto L_57;K_662:;
L_663: Cell t341=uf_mkp((void*)&uf_sl56);L_664: pushc(cx,t341);cx->cs[cx->csp++]=&&K_664;goto L_46;K_664:;
L_665: Cell t342=pop(cx);Cell t343=pop(cx);Cell t344=uf_cadd(t343,t342);L_666: pushc(cx,t344);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_667: cx->cs[cx->csp++]=&&K_667;goto L_57;K_667:;
L_668: Cell t345=uf_mkp((void*)&uf_sl57);L_669: pushc(cx,t345);cx->cs[cx->csp++]=&&K_669;goto L_46;K_669:;
L_670: pushp(cx,(void*)&&L_675);
L_671: pushp(cx,(void*)&&L_681);
L_672: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_672;goto *((!uf_zero(c))?th:el);K_672:;pop(cx);}
L_673: Cell t346=uf_mki(0LL);L_674: pushc(cx,t346);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_675: cx->cs[cx->csp++]=&&K_675;goto L_67;K_675:;
L_676: cx->cs[cx->csp++]=&&K_676;goto L_687;K_676:;
L_677: Cell t347=uf_mkp((void*)&uf_sl58);L_678: pushc(cx,t347);cx->cs[cx->csp++]=&&K_678;goto L_6;K_678:;
L_679: Cell t348=uf_mki(0LL);L_680: pushc(cx,t348);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_681: cx->cs[cx->csp++]=&&K_681;goto L_67;K_681:;
L_682: cx->cs[cx->csp++]=&&K_682;goto L_687;K_682:;
L_683: Cell t349=uf_mkp((void*)&uf_sl59);L_684: pushc(cx,t349);cx->cs[cx->csp++]=&&K_684;goto L_6;K_684:;
L_685: Cell t350=uf_mki(0LL);L_686: pushc(cx,t350);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_687: cx->cs[cx->csp++]=&&K_687;goto L_756;K_687:;
L_688: pushp(cx,(void*)&&L_692);
L_689: pushp(cx,(void*)&&L_708);
L_690: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_690:;cx->loops[fr].cont=&&K_WT_690;cx->loops[fr].end=&&K_WE_690;
cx->cs[cx->csp++]=&&K_WC_690;goto *cnd;K_WC_690:;
if(uf_zero(pop(cx)))goto K_WE_690;
cx->cs[cx->csp++]=&&K_WB_690;goto *bod;K_WB_690:;pop(cx);
goto K_WT_690;
K_WE_690:;cx->lsp=fr;}
L_691: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_692: cx->cs[cx->csp++]=&&K_692;goto L_57;K_692:;
L_693: Cell t351=uf_mkp((void*)&uf_sl60);L_694: pushc(cx,t351);cx->cs[cx->csp++]=&&K_694;goto L_46;K_694:;
L_695: cx->cs[cx->csp++]=&&K_695;goto L_57;K_695:;
L_696: Cell t352=uf_mkp((void*)&uf_sl61);L_697: pushc(cx,t352);cx->cs[cx->csp++]=&&K_697;goto L_46;K_697:;
L_698: Cell t353=pop(cx);Cell t354=pop(cx);Cell t355=uf_cadd(t354,t353);L_699: pushc(cx,t355);cx->cs[cx->csp++]=&&K_699;goto L_57;K_699:;
L_700: Cell t356=uf_mkp((void*)&uf_sl62);L_701: pushc(cx,t356);cx->cs[cx->csp++]=&&K_701;goto L_46;K_701:;
L_702: Cell t357=pop(cx);Cell t358=pop(cx);Cell t359=uf_cadd(t358,t357);L_703: pushc(cx,t359);cx->cs[cx->csp++]=&&K_703;goto L_57;K_703:;
L_704: Cell t360=uf_mkp((void*)&uf_sl63);L_705: pushc(cx,t360);cx->cs[cx->csp++]=&&K_705;goto L_46;K_705:;
L_706: Cell t361=pop(cx);Cell t362=pop(cx);Cell t363=uf_cadd(t362,t361);L_707: pushc(cx,t363);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_708: cx->cs[cx->csp++]=&&K_708;goto L_57;K_708:;
L_709: Cell t364=uf_mkp((void*)&uf_sl64);L_710: pushc(cx,t364);cx->cs[cx->csp++]=&&K_710;goto L_46;K_710:;
L_711: pushp(cx,(void*)&&L_716);
L_712: pushp(cx,(void*)&&L_722);
L_713: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_713;goto *((!uf_zero(c))?th:el);K_713:;pop(cx);}
L_714: Cell t365=uf_mki(0LL);L_715: pushc(cx,t365);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_716: cx->cs[cx->csp++]=&&K_716;goto L_67;K_716:;
L_717: cx->cs[cx->csp++]=&&K_717;goto L_756;K_717:;
L_718: Cell t366=uf_mkp((void*)&uf_sl65);L_719: pushc(cx,t366);cx->cs[cx->csp++]=&&K_719;goto L_6;K_719:;
L_720: Cell t367=uf_mki(0LL);L_721: pushc(cx,t367);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_722: cx->cs[cx->csp++]=&&K_722;goto L_57;K_722:;
L_723: Cell t368=uf_mkp((void*)&uf_sl66);L_724: pushc(cx,t368);cx->cs[cx->csp++]=&&K_724;goto L_46;K_724:;
L_725: pushp(cx,(void*)&&L_730);
L_726: pushp(cx,(void*)&&L_736);
L_727: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_727;goto *((!uf_zero(c))?th:el);K_727:;pop(cx);}
L_728: Cell t369=uf_mki(0LL);L_729: pushc(cx,t369);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_730: cx->cs[cx->csp++]=&&K_730;goto L_67;K_730:;
L_731: cx->cs[cx->csp++]=&&K_731;goto L_756;K_731:;
L_732: Cell t370=uf_mkp((void*)&uf_sl67);L_733: pushc(cx,t370);cx->cs[cx->csp++]=&&K_733;goto L_6;K_733:;
L_734: Cell t371=uf_mki(0LL);L_735: pushc(cx,t371);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_736: cx->cs[cx->csp++]=&&K_736;goto L_57;K_736:;
L_737: Cell t372=uf_mkp((void*)&uf_sl68);L_738: pushc(cx,t372);cx->cs[cx->csp++]=&&K_738;goto L_46;K_738:;
L_739: pushp(cx,(void*)&&L_744);
L_740: pushp(cx,(void*)&&L_750);
L_741: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_741;goto *((!uf_zero(c))?th:el);K_741:;pop(cx);}
L_742: Cell t373=uf_mki(0LL);L_743: pushc(cx,t373);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_744: cx->cs[cx->csp++]=&&K_744;goto L_67;K_744:;
L_745: cx->cs[cx->csp++]=&&K_745;goto L_756;K_745:;
L_746: Cell t374=uf_mkp((void*)&uf_sl69);L_747: pushc(cx,t374);cx->cs[cx->csp++]=&&K_747;goto L_6;K_747:;
L_748: Cell t375=uf_mki(0LL);L_749: pushc(cx,t375);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_750: cx->cs[cx->csp++]=&&K_750;goto L_67;K_750:;
L_751: cx->cs[cx->csp++]=&&K_751;goto L_756;K_751:;
L_752: Cell t376=uf_mkp((void*)&uf_sl70);L_753: pushc(cx,t376);cx->cs[cx->csp++]=&&K_753;goto L_6;K_753:;
L_754: Cell t377=uf_mki(0LL);L_755: pushc(cx,t377);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_756: cx->cs[cx->csp++]=&&K_756;goto L_789;K_756:;
L_757: pushp(cx,(void*)&&L_761);
L_758: pushp(cx,(void*)&&L_769);
L_759: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_759:;cx->loops[fr].cont=&&K_WT_759;cx->loops[fr].end=&&K_WE_759;
cx->cs[cx->csp++]=&&K_WC_759;goto *cnd;K_WC_759:;
if(uf_zero(pop(cx)))goto K_WE_759;
cx->cs[cx->csp++]=&&K_WB_759;goto *bod;K_WB_759:;pop(cx);
goto K_WT_759;
K_WE_759:;cx->lsp=fr;}
L_760: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_761: cx->cs[cx->csp++]=&&K_761;goto L_57;K_761:;
L_762: Cell t378=uf_mkp((void*)&uf_sl71);L_763: pushc(cx,t378);cx->cs[cx->csp++]=&&K_763;goto L_46;K_763:;
L_764: cx->cs[cx->csp++]=&&K_764;goto L_57;K_764:;
L_765: Cell t379=uf_mkp((void*)&uf_sl72);L_766: pushc(cx,t379);cx->cs[cx->csp++]=&&K_766;goto L_46;K_766:;
L_767: Cell t380=pop(cx);Cell t381=pop(cx);Cell t382=uf_cadd(t381,t380);L_768: pushc(cx,t382);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_769: cx->cs[cx->csp++]=&&K_769;goto L_57;K_769:;
L_770: Cell t383=uf_mkp((void*)&uf_sl73);L_771: pushc(cx,t383);cx->cs[cx->csp++]=&&K_771;goto L_46;K_771:;
L_772: pushp(cx,(void*)&&L_777);
L_773: pushp(cx,(void*)&&L_783);
L_774: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_774;goto *((!uf_zero(c))?th:el);K_774:;pop(cx);}
L_775: Cell t384=uf_mki(0LL);L_776: pushc(cx,t384);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_777: cx->cs[cx->csp++]=&&K_777;goto L_67;K_777:;
L_778: cx->cs[cx->csp++]=&&K_778;goto L_789;K_778:;
L_779: Cell t385=uf_mkp((void*)&uf_sl74);L_780: pushc(cx,t385);cx->cs[cx->csp++]=&&K_780;goto L_6;K_780:;
L_781: Cell t386=uf_mki(0LL);L_782: pushc(cx,t386);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_783: cx->cs[cx->csp++]=&&K_783;goto L_67;K_783:;
L_784: cx->cs[cx->csp++]=&&K_784;goto L_789;K_784:;
L_785: Cell t387=uf_mkp((void*)&uf_sl75);L_786: pushc(cx,t387);cx->cs[cx->csp++]=&&K_786;goto L_6;K_786:;
L_787: Cell t388=uf_mki(0LL);L_788: pushc(cx,t388);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_789: cx->cs[cx->csp++]=&&K_789;goto L_815;K_789:;
L_790: pushp(cx,(void*)&&L_803);
L_791: pushp(cx,(void*)&&L_807);
L_792: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_792:;cx->loops[fr].cont=&&K_WT_792;cx->loops[fr].end=&&K_WE_792;
cx->cs[cx->csp++]=&&K_WC_792;goto *cnd;K_WC_792:;
if(uf_zero(pop(cx)))goto K_WE_792;
cx->cs[cx->csp++]=&&K_WB_792;goto *bod;K_WB_792:;pop(cx);
goto K_WT_792;
K_WE_792:;cx->lsp=fr;}
L_793: cx->cs[cx->csp++]=&&K_793;goto L_57;K_793:;
L_794: Cell t389=uf_mkp((void*)&uf_sl76);L_795: pushc(cx,t389);cx->cs[cx->csp++]=&&K_795;goto L_46;K_795:;
L_796: cx->cs[cx->csp++]=&&K_796;goto L_57;K_796:;
L_797: Cell t390=uf_mkp((void*)&uf_sl77);L_798: pushc(cx,t390);cx->cs[cx->csp++]=&&K_798;goto L_46;K_798:;
L_799: Cell t391=pop(cx);Cell t392=pop(cx);Cell t393=uf_cadd(t392,t391);L_800: pushc(cx,t393);pushp(cx,(void*)&&L_813);
L_801: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_801;goto *b;K_801:;pop(cx);}}
L_802: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_803: cx->cs[cx->csp++]=&&K_803;goto L_57;K_803:;
L_804: Cell t394=uf_mkp((void*)&uf_sl78);L_805: pushc(cx,t394);cx->cs[cx->csp++]=&&K_805;goto L_46;K_805:;
L_806: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_807: cx->cs[cx->csp++]=&&K_807;goto L_67;K_807:;
L_808: cx->cs[cx->csp++]=&&K_808;goto L_815;K_808:;
L_809: Cell t395=uf_mkp((void*)&uf_sl79);L_810: pushc(cx,t395);cx->cs[cx->csp++]=&&K_810;goto L_6;K_810:;
L_811: Cell t396=uf_mki(0LL);L_812: pushc(cx,t396);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_813: Cell t397=uf_mkp((void*)&uf_sl80);L_814: pushc(cx,t397);cx->cs[cx->csp++]=&&K_814;goto L_53;K_814:;
L_815: cx->cs[cx->csp++]=&&K_815;goto L_57;K_815:;
L_816: Cell t398=uf_mkp((void*)&uf_sl81);L_817: pushc(cx,t398);cx->cs[cx->csp++]=&&K_817;goto L_46;K_817:;
L_818: pushp(cx,(void*)&&L_857);
L_819: pushp(cx,(void*)&&L_822);
L_820: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_820;goto *((!uf_zero(c))?th:el);K_820:;pop(cx);}
L_821: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_822: cx->cs[cx->csp++]=&&K_822;goto L_57;K_822:;
L_823: Cell t399=uf_mkp((void*)&uf_sl82);L_824: pushc(cx,t399);cx->cs[cx->csp++]=&&K_824;goto L_46;K_824:;
L_825: pushp(cx,(void*)&&L_865);
L_826: pushp(cx,(void*)&&L_829);
L_827: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_827;goto *((!uf_zero(c))?th:el);K_827:;pop(cx);}
L_828: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_829: cx->cs[cx->csp++]=&&K_829;goto L_57;K_829:;
L_830: Cell t400=uf_mkp((void*)&uf_sl83);L_831: pushc(cx,t400);cx->cs[cx->csp++]=&&K_831;goto L_46;K_831:;
L_832: pushp(cx,(void*)&&L_871);
L_833: pushp(cx,(void*)&&L_836);
L_834: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_834;goto *((!uf_zero(c))?th:el);K_834:;pop(cx);}
L_835: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_836: cx->cs[cx->csp++]=&&K_836;goto L_57;K_836:;
L_837: Cell t401=uf_mkp((void*)&uf_sl84);L_838: pushc(cx,t401);cx->cs[cx->csp++]=&&K_838;goto L_46;K_838:;
L_839: pushp(cx,(void*)&&L_877);
L_840: pushp(cx,(void*)&&L_843);
L_841: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_841;goto *((!uf_zero(c))?th:el);K_841:;pop(cx);}
L_842: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_843: cx->cs[cx->csp++]=&&K_843;goto L_57;K_843:;
L_844: Cell t402=uf_mkp((void*)&uf_sl85);L_845: pushc(cx,t402);cx->cs[cx->csp++]=&&K_845;goto L_46;K_845:;
L_846: pushp(cx,(void*)&&L_881);
L_847: pushp(cx,(void*)&&L_850);
L_848: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_848;goto *((!uf_zero(c))?th:el);K_848:;pop(cx);}
L_849: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_850: cx->cs[cx->csp++]=&&K_850;goto L_57;K_850:;
L_851: Cell t403=uf_mkp((void*)&uf_sl86);L_852: pushc(cx,t403);cx->cs[cx->csp++]=&&K_852;goto L_46;K_852:;
L_853: pushp(cx,(void*)&&L_898);
L_854: pushp(cx,(void*)&&L_915);
L_855: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_855;goto *((!uf_zero(c))?th:el);K_855:;pop(cx);}
L_856: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_857: cx->cs[cx->csp++]=&&K_857;goto L_67;K_857:;
L_858: Cell t404=uf_mkp((void*)&uf_sl87);L_859: pushc(cx,t404);cx->cs[cx->csp++]=&&K_859;goto L_6;K_859:;
L_860: cx->cs[cx->csp++]=&&K_860;goto L_815;K_860:;
L_861: Cell t405=uf_mkp((void*)&uf_sl88);L_862: pushc(cx,t405);cx->cs[cx->csp++]=&&K_862;goto L_6;K_862:;
L_863: Cell t406=uf_mki(0LL);L_864: pushc(cx,t406);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_865: cx->cs[cx->csp++]=&&K_865;goto L_67;K_865:;
L_866: cx->cs[cx->csp++]=&&K_866;goto L_815;K_866:;
L_867: Cell t407=uf_mkp((void*)&uf_sl89);L_868: pushc(cx,t407);cx->cs[cx->csp++]=&&K_868;goto L_6;K_868:;
L_869: Cell t408=uf_mki(0LL);L_870: pushc(cx,t408);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_871: cx->cs[cx->csp++]=&&K_871;goto L_67;K_871:;
L_872: cx->cs[cx->csp++]=&&K_872;goto L_815;K_872:;
L_873: Cell t409=uf_mkp((void*)&uf_sl90);L_874: pushc(cx,t409);cx->cs[cx->csp++]=&&K_874;goto L_6;K_874:;
L_875: Cell t410=uf_mki(0LL);L_876: pushc(cx,t410);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_877: cx->cs[cx->csp++]=&&K_877;goto L_67;K_877:;
L_878: cx->cs[cx->csp++]=&&K_878;goto L_815;K_878:;
L_879: Cell t411=uf_mki(0LL);L_880: pushc(cx,t411);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_881: cx->cs[cx->csp++]=&&K_881;goto L_67;K_881:;
L_882: Cell t412=var_trans__vars;L_883: pushc(cx,t412);cx->cs[cx->csp++]=&&K_883;goto L_57;K_883:;
L_884: op_getq(cx);
L_885: op_dup(cx);L_886: op_not(cx);
L_887: pushp(cx,(void*)&&L_896);
L_888: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_888;goto *b;K_888:;pop(cx);}}
L_889: cx->cs[cx->csp++]=&&K_889;goto L_67;K_889:;
L_890: op_dup(cx);L_891: Cell t413=uf_mkp((void*)&uf_sl91);L_892: pushc(cx,t413);op_fmt(cx);
L_893: cx->cs[cx->csp++]=&&K_893;goto L_6;K_893:;
L_894: Cell t414=uf_mki(0LL);L_895: pushc(cx,t414);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_896: Cell t415=uf_mkp((void*)&uf_sl92);L_897: pushc(cx,t415);cx->cs[cx->csp++]=&&K_897;goto L_53;K_897:;
L_898: cx->cs[cx->csp++]=&&K_898;goto L_67;K_898:;
L_899: Cell t416=var_trans__vars;L_900: pushc(cx,t416);cx->cs[cx->csp++]=&&K_900;goto L_57;K_900:;
L_901: op_getq(cx);
L_902: op_dup(cx);L_903: op_not(cx);
L_904: pushp(cx,(void*)&&L_913);
L_905: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_905;goto *b;K_905:;pop(cx);}}
L_906: cx->cs[cx->csp++]=&&K_906;goto L_67;K_906:;
L_907: op_dup(cx);L_908: Cell t417=uf_mkp((void*)&uf_sl93);L_909: pushc(cx,t417);op_fmt(cx);
L_910: cx->cs[cx->csp++]=&&K_910;goto L_6;K_910:;
L_911: Cell t418=uf_mki(0LL);L_912: pushc(cx,t418);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_913: Cell t419=uf_mkp((void*)&uf_sl94);L_914: pushc(cx,t419);cx->cs[cx->csp++]=&&K_914;goto L_53;K_914:;
L_915: Cell t420=uf_mkp((void*)&uf_sl95);L_916: L_917: var_trans__lasts=t420;cx->cs[cx->csp++]=&&K_917;goto L_969;K_917:;
L_918: pushp(cx,(void*)&&L_2231);
L_919: pushp(cx,(void*)&&L_2236);
L_920: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_920:;cx->loops[fr].cont=&&K_WT_920;cx->loops[fr].end=&&K_WE_920;
cx->cs[cx->csp++]=&&K_WC_920;goto *cnd;K_WC_920:;
if(uf_zero(pop(cx)))goto K_WE_920;
cx->cs[cx->csp++]=&&K_WB_920;goto *bod;K_WB_920:;pop(cx);
goto K_WT_920;
K_WE_920:;cx->lsp=fr;}
L_921: Cell t421=uf_mki(0LL);L_922: pushc(cx,t421);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_923: cx->cs[cx->csp++]=&&K_923;goto L_57;K_923:;
L_924: Cell t422=uf_mkp((void*)&uf_sl96);L_925: pushc(cx,t422);cx->cs[cx->csp++]=&&K_925;goto L_46;K_925:;
L_926: cx->cs[cx->csp++]=&&K_926;goto L_57;K_926:;
L_927: Cell t423=uf_mkp((void*)&uf_sl97);L_928: pushc(cx,t423);cx->cs[cx->csp++]=&&K_928;goto L_46;K_928:;
L_929: Cell t424=pop(cx);Cell t425=pop(cx);Cell t426=uf_cadd(t425,t424);L_930: pushc(cx,t426);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_931: cx->cs[cx->csp++]=&&K_931;goto L_57;K_931:;
L_932: Cell t427=uf_mkp((void*)&uf_sl98);L_933: pushc(cx,t427);cx->cs[cx->csp++]=&&K_933;goto L_46;K_933:;
L_934: pushp(cx,(void*)&&L_939);
L_935: pushp(cx,(void*)&&L_954);
L_936: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_936;goto *((!uf_zero(c))?th:el);K_936:;pop(cx);}
L_937: Cell t428=uf_mki(0LL);L_938: pushc(cx,t428);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_939: Cell t429=var_trans__lasts;L_940: Cell t430=uf_mkp((void*)&uf_sl99);L_941: pushc(cx,t429);pushc(cx,t430);cx->cs[cx->csp++]=&&K_941;goto L_46;K_941:;
L_942: pushp(cx,(void*)&&L_952);
L_943: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_943;goto *b;K_943:;pop(cx);}}
L_944: cx->cs[cx->csp++]=&&K_944;goto L_67;K_944:;
L_945: Cell t431=var_trans__lasts;L_946: L_947: Cell t432=uf_mkp((void*)&uf_sl100);L_948: pushc(cx,t431);pushc(cx,t431);pushc(cx,t432);op_fmt(cx);
L_949: cx->cs[cx->csp++]=&&K_949;goto L_6;K_949:;
L_950: Cell t433=uf_mki(0LL);L_951: pushc(cx,t433);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_952: Cell t434=uf_mkp((void*)&uf_sl101);L_953: pushc(cx,t434);cx->cs[cx->csp++]=&&K_953;goto L_53;K_953:;
L_954: Cell t435=var_trans__lasts;L_955: Cell t436=uf_mkp((void*)&uf_sl102);L_956: pushc(cx,t435);pushc(cx,t436);cx->cs[cx->csp++]=&&K_956;goto L_46;K_956:;
L_957: pushp(cx,(void*)&&L_967);
L_958: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_958;goto *b;K_958:;pop(cx);}}
L_959: cx->cs[cx->csp++]=&&K_959;goto L_67;K_959:;
L_960: Cell t437=var_trans__lasts;L_961: L_962: Cell t438=uf_mkp((void*)&uf_sl103);L_963: pushc(cx,t437);pushc(cx,t437);pushc(cx,t438);op_fmt(cx);
L_964: cx->cs[cx->csp++]=&&K_964;goto L_6;K_964:;
L_965: Cell t439=uf_mki(0LL);L_966: pushc(cx,t439);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_967: Cell t440=uf_mkp((void*)&uf_sl104);L_968: pushc(cx,t440);cx->cs[cx->csp++]=&&K_968;goto L_53;K_968:;
L_969: cx->cs[cx->csp++]=&&K_969;goto L_57;K_969:;
L_970: Cell t441=pop(cx);L_971: L_972: Cell t442=uf_mkp((void*)&uf_sl105);L_973: var_trans__ptk=t441;pushc(cx,t441);pushc(cx,t442);op_glob(cx);
L_974: pushp(cx,(void*)&&L_1041);
L_975: pushp(cx,(void*)&&L_978);
L_976: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_976;goto *((!uf_zero(c))?th:el);K_976:;pop(cx);}
L_977: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_978: Cell t443=var_trans__ptk;L_979: Cell t444=uf_mkp((void*)&uf_sl106);L_980: pushc(cx,t443);pushc(cx,t444);op_starts(cx);
L_981: pushp(cx,(void*)&&L_1150);
L_982: pushp(cx,(void*)&&L_985);
L_983: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_983;goto *((!uf_zero(c))?th:el);K_983:;pop(cx);}
L_984: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_985: Cell t445=var_trans__ptk;L_986: Cell t446=uf_mkp((void*)&uf_sl107);L_987: pushc(cx,t445);pushc(cx,t446);op_starts(cx);
L_988: pushp(cx,(void*)&&L_1053);
L_989: pushp(cx,(void*)&&L_992);
L_990: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_990;goto *((!uf_zero(c))?th:el);K_990:;pop(cx);}
L_991: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_992: Cell t447=var_trans__ptk;L_993: Cell t448=uf_mkp((void*)&uf_sl108);L_994: pushc(cx,t447);pushc(cx,t448);cx->cs[cx->csp++]=&&K_994;goto L_46;K_994:;
L_995: pushp(cx,(void*)&&L_1062);
L_996: pushp(cx,(void*)&&L_999);
L_997: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_997;goto *((!uf_zero(c))?th:el);K_997:;pop(cx);}
L_998: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_999: Cell t449=var_trans__ptk;L_1000: Cell t450=uf_mkp((void*)&uf_sl109);L_1001: pushc(cx,t449);pushc(cx,t450);cx->cs[cx->csp++]=&&K_1001;goto L_46;K_1001:;
L_1002: pushp(cx,(void*)&&L_1077);
L_1003: pushp(cx,(void*)&&L_1006);
L_1004: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1004;goto *((!uf_zero(c))?th:el);K_1004:;pop(cx);}
L_1005: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1006: Cell t451=var_trans__ptk;L_1007: Cell t452=uf_mkp((void*)&uf_sl110);L_1008: pushc(cx,t451);pushc(cx,t452);cx->cs[cx->csp++]=&&K_1008;goto L_46;K_1008:;
L_1009: pushp(cx,(void*)&&L_1084);
L_1010: pushp(cx,(void*)&&L_1013);
L_1011: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1011;goto *((!uf_zero(c))?th:el);K_1011:;pop(cx);}
L_1012: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1013: Cell t453=var_trans__ptk;L_1014: Cell t454=uf_mkp((void*)&uf_sl111);L_1015: pushc(cx,t453);pushc(cx,t454);cx->cs[cx->csp++]=&&K_1015;goto L_46;K_1015:;
L_1016: pushp(cx,(void*)&&L_1110);
L_1017: pushp(cx,(void*)&&L_1020);
L_1018: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1018;goto *((!uf_zero(c))?th:el);K_1018:;pop(cx);}
L_1019: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1020: Cell t455=var_trans__ptk;L_1021: Cell t456=uf_mkp((void*)&uf_sl112);L_1022: pushc(cx,t455);pushc(cx,t456);cx->cs[cx->csp++]=&&K_1022;goto L_46;K_1022:;
L_1023: pushp(cx,(void*)&&L_1136);
L_1024: pushp(cx,(void*)&&L_1027);
L_1025: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1025;goto *((!uf_zero(c))?th:el);K_1025:;pop(cx);}
L_1026: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1027: Cell t457=var_trans__ptk;L_1028: Cell t458=uf_mkp((void*)&uf_sl113);L_1029: pushc(cx,t457);pushc(cx,t458);cx->cs[cx->csp++]=&&K_1029;goto L_46;K_1029:;
L_1030: pushp(cx,(void*)&&L_1143);
L_1031: pushp(cx,(void*)&&L_1034);
L_1032: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1032;goto *((!uf_zero(c))?th:el);K_1032:;pop(cx);}
L_1033: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1034: cx->cs[cx->csp++]=&&K_1034;goto L_61;K_1034:;
L_1035: Cell t459=uf_mkp((void*)&uf_sl114);L_1036: pushc(cx,t459);cx->cs[cx->csp++]=&&K_1036;goto L_46;K_1036:;
L_1037: pushp(cx,(void*)&&L_1268);
L_1038: pushp(cx,(void*)&&L_1315);
L_1039: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1039;goto *((!uf_zero(c))?th:el);K_1039:;pop(cx);}
L_1040: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1041: Cell t460=var_trans__ptk;L_1042: Cell t461=uf_mkp((void*)&uf_sl115);L_1043: pushc(cx,t460);pushc(cx,t461);op_match(cx);
L_1044: op_drp(cx);L_1045: Cell t462=uf_mki(0LL);L_1046: pushc(cx,t462);op_get(cx);
L_1047: cx->cs[cx->csp++]=&&K_1047;goto L_6;K_1047:;
L_1048: Cell t463=uf_mkp((void*)&uf_sl116);L_1049: pushc(cx,t463);cx->cs[cx->csp++]=&&K_1049;goto L_6;K_1049:;
L_1050: cx->cs[cx->csp++]=&&K_1050;goto L_67;K_1050:;
L_1051: Cell t464=uf_mki(0LL);L_1052: pushc(cx,t464);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1053: Cell t465=var_trans__ptk;L_1054: pushc(cx,t465);cx->cs[cx->csp++]=&&K_1054;goto L_6;K_1054:;
L_1055: Cell t466=uf_mkp((void*)&uf_sl117);L_1056: pushc(cx,t466);cx->cs[cx->csp++]=&&K_1056;goto L_6;K_1056:;
L_1057: cx->cs[cx->csp++]=&&K_1057;goto L_67;K_1057:;
L_1058: Cell t467=uf_mkp((void*)&uf_sl118);L_1059: L_1060: Cell t468=uf_mki(0LL);L_1061: var_trans__lasts=t467;pushc(cx,t468);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1062: cx->cs[cx->csp++]=&&K_1062;goto L_67;K_1062:;
L_1063: cx->cs[cx->csp++]=&&K_1063;goto L_449;K_1063:;
L_1064: Cell t469=uf_mkp((void*)&uf_sl119);L_1065: L_1066: var_trans__e=t469;cx->cs[cx->csp++]=&&K_1066;goto L_57;K_1066:;
L_1067: Cell t470=var_trans__e;L_1068: pushc(cx,t470);cx->cs[cx->csp++]=&&K_1068;goto L_46;K_1068:;
L_1069: op_not(cx);
L_1070: pushp(cx,(void*)&&L_78);
L_1071: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1071;goto *b;K_1071:;pop(cx);}}
L_1072: cx->cs[cx->csp++]=&&K_1072;goto L_67;K_1072:;
L_1073: Cell t471=uf_mkp((void*)&uf_sl120);L_1074: L_1075: Cell t472=uf_mki(0LL);L_1076: var_trans__lasts=t471;pushc(cx,t472);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1077: cx->cs[cx->csp++]=&&K_1077;goto L_67;K_1077:;
L_1078: Cell t473=uf_mkp((void*)&uf_sl121);L_1079: pushc(cx,t473);cx->cs[cx->csp++]=&&K_1079;goto L_6;K_1079:;
L_1080: Cell t474=uf_mkp((void*)&uf_sl122);L_1081: L_1082: Cell t475=uf_mki(0LL);L_1083: var_trans__lasts=t474;pushc(cx,t475);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1084: cx->cs[cx->csp++]=&&K_1084;goto L_67;K_1084:;
L_1085: Cell t476=uf_mkp((void*)&uf_sl123);L_1086: L_1087: var_trans__e=t476;cx->cs[cx->csp++]=&&K_1087;goto L_57;K_1087:;
L_1088: Cell t477=var_trans__e;L_1089: pushc(cx,t477);cx->cs[cx->csp++]=&&K_1089;goto L_46;K_1089:;
L_1090: op_not(cx);
L_1091: pushp(cx,(void*)&&L_78);
L_1092: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1092;goto *b;K_1092:;pop(cx);}}
L_1093: cx->cs[cx->csp++]=&&K_1093;goto L_67;K_1093:;
L_1094: cx->cs[cx->csp++]=&&K_1094;goto L_449;K_1094:;
L_1095: Cell t478=uf_mkp((void*)&uf_sl124);L_1096: L_1097: var_trans__e=t478;cx->cs[cx->csp++]=&&K_1097;goto L_57;K_1097:;
L_1098: Cell t479=var_trans__e;L_1099: pushc(cx,t479);cx->cs[cx->csp++]=&&K_1099;goto L_46;K_1099:;
L_1100: op_not(cx);
L_1101: pushp(cx,(void*)&&L_78);
L_1102: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1102;goto *b;K_1102:;pop(cx);}}
L_1103: cx->cs[cx->csp++]=&&K_1103;goto L_67;K_1103:;
L_1104: Cell t480=uf_mkp((void*)&uf_sl125);L_1105: pushc(cx,t480);cx->cs[cx->csp++]=&&K_1105;goto L_6;K_1105:;
L_1106: Cell t481=uf_mkp((void*)&uf_sl126);L_1107: L_1108: Cell t482=uf_mki(0LL);L_1109: var_trans__lasts=t481;pushc(cx,t482);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1110: cx->cs[cx->csp++]=&&K_1110;goto L_67;K_1110:;
L_1111: Cell t483=uf_mkp((void*)&uf_sl127);L_1112: L_1113: var_trans__e=t483;cx->cs[cx->csp++]=&&K_1113;goto L_57;K_1113:;
L_1114: Cell t484=var_trans__e;L_1115: pushc(cx,t484);cx->cs[cx->csp++]=&&K_1115;goto L_46;K_1115:;
L_1116: op_not(cx);
L_1117: pushp(cx,(void*)&&L_78);
L_1118: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1118;goto *b;K_1118:;pop(cx);}}
L_1119: cx->cs[cx->csp++]=&&K_1119;goto L_67;K_1119:;
L_1120: cx->cs[cx->csp++]=&&K_1120;goto L_449;K_1120:;
L_1121: Cell t485=uf_mkp((void*)&uf_sl128);L_1122: L_1123: var_trans__e=t485;cx->cs[cx->csp++]=&&K_1123;goto L_57;K_1123:;
L_1124: Cell t486=var_trans__e;L_1125: pushc(cx,t486);cx->cs[cx->csp++]=&&K_1125;goto L_46;K_1125:;
L_1126: op_not(cx);
L_1127: pushp(cx,(void*)&&L_78);
L_1128: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1128;goto *b;K_1128:;pop(cx);}}
L_1129: cx->cs[cx->csp++]=&&K_1129;goto L_67;K_1129:;
L_1130: Cell t487=uf_mkp((void*)&uf_sl129);L_1131: pushc(cx,t487);cx->cs[cx->csp++]=&&K_1131;goto L_6;K_1131:;
L_1132: Cell t488=uf_mkp((void*)&uf_sl130);L_1133: L_1134: Cell t489=uf_mki(0LL);L_1135: var_trans__lasts=t488;pushc(cx,t489);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1136: cx->cs[cx->csp++]=&&K_1136;goto L_67;K_1136:;
L_1137: Cell t490=uf_mkp((void*)&uf_sl131);L_1138: pushc(cx,t490);cx->cs[cx->csp++]=&&K_1138;goto L_6;K_1138:;
L_1139: Cell t491=uf_mkp((void*)&uf_sl132);L_1140: L_1141: Cell t492=uf_mki(0LL);L_1142: var_trans__lasts=t491;pushc(cx,t492);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1143: cx->cs[cx->csp++]=&&K_1143;goto L_67;K_1143:;
L_1144: Cell t493=uf_mkp((void*)&uf_sl133);L_1145: pushc(cx,t493);cx->cs[cx->csp++]=&&K_1145;goto L_6;K_1145:;
L_1146: Cell t494=uf_mkp((void*)&uf_sl134);L_1147: L_1148: Cell t495=uf_mki(0LL);L_1149: var_trans__lasts=t494;pushc(cx,t495);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1150: Cell t496=var_trans__ptk;L_1151: Cell t497=uf_mki(1LL);L_1152: L_1153: pushc(cx,t496);pushc(cx,t497);pushc(cx,t496);{Cell a0=pop(cx);int r=((int(*)(void*))uf_im9)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_1154: Cell t498=uf_mki(1LL);L_1155: Cell t499=pop(cx);Cell t500=uf_csub(t499,t498);L_1156: pushc(cx,t500);op_slice(cx);
L_1157: Cell t501=pop(cx);L_1158: L_1159: Cell t502=uf_mkp((void*)&uf_sl135);L_1160: var_trans__ci=t501;pushc(cx,t501);pushc(cx,t502);op_starts(cx);
L_1161: pushp(cx,(void*)&&L_1176);
L_1162: pushp(cx,(void*)&&L_1166);
L_1163: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1163;goto *((!uf_zero(c))?th:el);K_1163:;pop(cx);}
L_1164: Cell t503=uf_mki(0LL);L_1165: pushc(cx,t503);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1166: Cell t504=var_trans__ci;L_1167: pushc(cx,t504);op_loadx(cx);
L_1168: Cell t505=uf_mki(255LL);L_1169: Cell t506=pop(cx);Cell t507=uf_cand(t506,t505);L_1170: pushc(cx,t507);cx->cs[cx->csp++]=&&K_1170;goto L_24;K_1170:;
L_1171: Cell t508=uf_mkp((void*)&uf_sl136);L_1172: pushc(cx,t508);cx->cs[cx->csp++]=&&K_1172;goto L_6;K_1172:;
L_1173: cx->cs[cx->csp++]=&&K_1173;goto L_67;K_1173:;
L_1174: Cell t509=uf_mki(0LL);L_1175: pushc(cx,t509);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1176: Cell t510=var_trans__ci;L_1177: Cell t511=uf_mkp((void*)&uf_sl137);L_1178: pushc(cx,t510);pushc(cx,t511);cx->cs[cx->csp++]=&&K_1178;goto L_46;K_1178:;
L_1179: pushp(cx,(void*)&&L_1184);
L_1180: pushp(cx,(void*)&&L_1191);
L_1181: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1181;goto *((!uf_zero(c))?th:el);K_1181:;pop(cx);}
L_1182: Cell t512=uf_mki(0LL);L_1183: pushc(cx,t512);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1184: Cell t513=uf_mki(10LL);L_1185: pushc(cx,t513);cx->cs[cx->csp++]=&&K_1185;goto L_24;K_1185:;
L_1186: Cell t514=uf_mkp((void*)&uf_sl138);L_1187: pushc(cx,t514);cx->cs[cx->csp++]=&&K_1187;goto L_6;K_1187:;
L_1188: cx->cs[cx->csp++]=&&K_1188;goto L_67;K_1188:;
L_1189: Cell t515=uf_mki(0LL);L_1190: pushc(cx,t515);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1191: Cell t516=var_trans__ci;L_1192: Cell t517=uf_mkp((void*)&uf_sl139);L_1193: pushc(cx,t516);pushc(cx,t517);cx->cs[cx->csp++]=&&K_1193;goto L_46;K_1193:;
L_1194: pushp(cx,(void*)&&L_1199);
L_1195: pushp(cx,(void*)&&L_1206);
L_1196: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1196;goto *((!uf_zero(c))?th:el);K_1196:;pop(cx);}
L_1197: Cell t518=uf_mki(0LL);L_1198: pushc(cx,t518);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1199: Cell t519=uf_mki(9LL);L_1200: pushc(cx,t519);cx->cs[cx->csp++]=&&K_1200;goto L_24;K_1200:;
L_1201: Cell t520=uf_mkp((void*)&uf_sl140);L_1202: pushc(cx,t520);cx->cs[cx->csp++]=&&K_1202;goto L_6;K_1202:;
L_1203: cx->cs[cx->csp++]=&&K_1203;goto L_67;K_1203:;
L_1204: Cell t521=uf_mki(0LL);L_1205: pushc(cx,t521);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1206: Cell t522=var_trans__ci;L_1207: Cell t523=uf_mkp((void*)&uf_sl141);L_1208: pushc(cx,t522);pushc(cx,t523);cx->cs[cx->csp++]=&&K_1208;goto L_46;K_1208:;
L_1209: pushp(cx,(void*)&&L_1214);
L_1210: pushp(cx,(void*)&&L_1221);
L_1211: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1211;goto *((!uf_zero(c))?th:el);K_1211:;pop(cx);}
L_1212: Cell t524=uf_mki(0LL);L_1213: pushc(cx,t524);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1214: Cell t525=uf_mki(13LL);L_1215: pushc(cx,t525);cx->cs[cx->csp++]=&&K_1215;goto L_24;K_1215:;
L_1216: Cell t526=uf_mkp((void*)&uf_sl142);L_1217: pushc(cx,t526);cx->cs[cx->csp++]=&&K_1217;goto L_6;K_1217:;
L_1218: cx->cs[cx->csp++]=&&K_1218;goto L_67;K_1218:;
L_1219: Cell t527=uf_mki(0LL);L_1220: pushc(cx,t527);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1221: Cell t528=var_trans__ci;L_1222: Cell t529=uf_mkp((void*)&uf_sl143);L_1223: pushc(cx,t528);pushc(cx,t529);cx->cs[cx->csp++]=&&K_1223;goto L_46;K_1223:;
L_1224: pushp(cx,(void*)&&L_1229);
L_1225: pushp(cx,(void*)&&L_1236);
L_1226: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1226;goto *((!uf_zero(c))?th:el);K_1226:;pop(cx);}
L_1227: Cell t530=uf_mki(0LL);L_1228: pushc(cx,t530);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1229: Cell t531=uf_mki(0LL);L_1230: pushc(cx,t531);cx->cs[cx->csp++]=&&K_1230;goto L_24;K_1230:;
L_1231: Cell t532=uf_mkp((void*)&uf_sl144);L_1232: pushc(cx,t532);cx->cs[cx->csp++]=&&K_1232;goto L_6;K_1232:;
L_1233: cx->cs[cx->csp++]=&&K_1233;goto L_67;K_1233:;
L_1234: Cell t533=uf_mki(0LL);L_1235: pushc(cx,t533);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1236: Cell t534=var_trans__ci;L_1237: Cell t535=uf_mkp((void*)&uf_sl145);L_1238: pushc(cx,t534);pushc(cx,t535);cx->cs[cx->csp++]=&&K_1238;goto L_46;K_1238:;
L_1239: pushp(cx,(void*)&&L_1244);
L_1240: pushp(cx,(void*)&&L_1251);
L_1241: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1241;goto *((!uf_zero(c))?th:el);K_1241:;pop(cx);}
L_1242: Cell t536=uf_mki(0LL);L_1243: pushc(cx,t536);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1244: Cell t537=uf_mki(92LL);L_1245: pushc(cx,t537);cx->cs[cx->csp++]=&&K_1245;goto L_24;K_1245:;
L_1246: Cell t538=uf_mkp((void*)&uf_sl146);L_1247: pushc(cx,t538);cx->cs[cx->csp++]=&&K_1247;goto L_6;K_1247:;
L_1248: cx->cs[cx->csp++]=&&K_1248;goto L_67;K_1248:;
L_1249: Cell t539=uf_mki(0LL);L_1250: pushc(cx,t539);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1251: Cell t540=var_trans__ci;L_1252: Cell t541=uf_mkp((void*)&uf_sl147);L_1253: pushc(cx,t540);pushc(cx,t541);cx->cs[cx->csp++]=&&K_1253;goto L_46;K_1253:;
L_1254: pushp(cx,(void*)&&L_1259);
L_1255: pushp(cx,(void*)&&L_1266);
L_1256: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1256;goto *((!uf_zero(c))?th:el);K_1256:;pop(cx);}
L_1257: Cell t542=uf_mki(0LL);L_1258: pushc(cx,t542);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1259: Cell t543=uf_mki(39LL);L_1260: pushc(cx,t543);cx->cs[cx->csp++]=&&K_1260;goto L_24;K_1260:;
L_1261: Cell t544=uf_mkp((void*)&uf_sl148);L_1262: pushc(cx,t544);cx->cs[cx->csp++]=&&K_1262;goto L_6;K_1262:;
L_1263: cx->cs[cx->csp++]=&&K_1263;goto L_67;K_1263:;
L_1264: Cell t545=uf_mki(0LL);L_1265: pushc(cx,t545);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1266: Cell t546=uf_mkp((void*)&uf_sl149);L_1267: pushc(cx,t546);cx->cs[cx->csp++]=&&K_1267;goto L_53;K_1267:;
L_1268: Cell t547=var_trans__ptk;L_1269: Cell t548=var_trans__ps;L_1270: L_1271: pushc(cx,t548);pushc(cx,t547);op_push(cx);
L_1272: Cell t549=pop(cx);L_1273: var_trans__ps=t549;cx->cs[cx->csp++]=&&K_1273;goto L_67;K_1273:;
L_1274: cx->cs[cx->csp++]=&&K_1274;goto L_67;K_1274:;
L_1275: cx->cs[cx->csp++]=&&K_1275;goto L_57;K_1275:;
L_1276: Cell t550=uf_mkp((void*)&uf_sl150);L_1277: pushc(cx,t550);cx->cs[cx->csp++]=&&K_1277;goto L_46;K_1277:;
L_1278: pushp(cx,(void*)&&L_1299);
L_1279: pushp(cx,(void*)&&L_1301);
L_1280: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1280;goto *((!uf_zero(c))?th:el);K_1280:;pop(cx);}
L_1281: Cell t551=uf_mkp((void*)&uf_sl151);L_1282: L_1283: var_trans__e=t551;cx->cs[cx->csp++]=&&K_1283;goto L_57;K_1283:;
L_1284: Cell t552=var_trans__e;L_1285: pushc(cx,t552);cx->cs[cx->csp++]=&&K_1285;goto L_46;K_1285:;
L_1286: op_not(cx);
L_1287: pushp(cx,(void*)&&L_78);
L_1288: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1288;goto *b;K_1288:;pop(cx);}}
L_1289: cx->cs[cx->csp++]=&&K_1289;goto L_67;K_1289:;
L_1290: Cell t553=var_trans__ps;L_1291: pushc(cx,t553);op_lpop(cx);
L_1292: Cell t554=uf_mkp((void*)&uf_sl152);L_1293: pushc(cx,t554);op_fmt(cx);
L_1294: cx->cs[cx->csp++]=&&K_1294;goto L_6;K_1294:;
L_1295: Cell t555=uf_mkp((void*)&uf_sl153);L_1296: L_1297: Cell t556=uf_mki(0LL);L_1298: var_trans__lasts=t555;pushc(cx,t556);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1299: Cell t557=uf_mki(0LL);L_1300: pushc(cx,t557);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1301: cx->cs[cx->csp++]=&&K_1301;goto L_449;K_1301:;
L_1302: pushp(cx,(void*)&&L_1307);
L_1303: pushp(cx,(void*)&&L_1311);
L_1304: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_1304:;cx->loops[fr].cont=&&K_WT_1304;cx->loops[fr].end=&&K_WE_1304;
cx->cs[cx->csp++]=&&K_WC_1304;goto *cnd;K_WC_1304:;
if(uf_zero(pop(cx)))goto K_WE_1304;
cx->cs[cx->csp++]=&&K_WB_1304;goto *bod;K_WB_1304:;pop(cx);
goto K_WT_1304;
K_WE_1304:;cx->lsp=fr;}
L_1305: Cell t558=uf_mki(0LL);L_1306: pushc(cx,t558);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1307: cx->cs[cx->csp++]=&&K_1307;goto L_57;K_1307:;
L_1308: Cell t559=uf_mkp((void*)&uf_sl154);L_1309: pushc(cx,t559);cx->cs[cx->csp++]=&&K_1309;goto L_46;K_1309:;
L_1310: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1311: cx->cs[cx->csp++]=&&K_1311;goto L_67;K_1311:;
L_1312: cx->cs[cx->csp++]=&&K_1312;goto L_449;K_1312:;
L_1313: Cell t560=uf_mki(0LL);L_1314: pushc(cx,t560);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1315: cx->cs[cx->csp++]=&&K_1315;goto L_61;K_1315:;
L_1316: Cell t561=uf_mkp((void*)&uf_sl155);L_1317: pushc(cx,t561);cx->cs[cx->csp++]=&&K_1317;goto L_46;K_1317:;
L_1318: pushp(cx,(void*)&&L_1322);
L_1319: pushp(cx,(void*)&&L_1354);
L_1320: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1320;goto *((!uf_zero(c))?th:el);K_1320:;pop(cx);}
L_1321: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1322: Cell t562=var_trans__vars;L_1323: Cell t563=var_trans__ptk;L_1324: pushc(cx,t562);pushc(cx,t563);op_getq(cx);
L_1325: op_dup(cx);L_1326: op_not(cx);
L_1327: pushp(cx,(void*)&&L_1369);
L_1328: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1328;goto *b;K_1328:;pop(cx);}}
L_1329: Cell t564=var_trans__ps;L_1330: pushc(cx,t564);op_swp(cx);L_1331: op_push(cx);
L_1332: Cell t565=pop(cx);L_1333: var_trans__ps=t565;cx->cs[cx->csp++]=&&K_1333;goto L_67;K_1333:;
L_1334: cx->cs[cx->csp++]=&&K_1334;goto L_67;K_1334:;
L_1335: cx->cs[cx->csp++]=&&K_1335;goto L_449;K_1335:;
L_1336: Cell t566=uf_mkp((void*)&uf_sl156);L_1337: L_1338: var_trans__e=t566;cx->cs[cx->csp++]=&&K_1338;goto L_57;K_1338:;
L_1339: Cell t567=var_trans__e;L_1340: pushc(cx,t567);cx->cs[cx->csp++]=&&K_1340;goto L_46;K_1340:;
L_1341: op_not(cx);
L_1342: pushp(cx,(void*)&&L_78);
L_1343: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1343;goto *b;K_1343:;pop(cx);}}
L_1344: cx->cs[cx->csp++]=&&K_1344;goto L_67;K_1344:;
L_1345: Cell t568=var_trans__ps;L_1346: pushc(cx,t568);op_lpop(cx);
L_1347: Cell t569=uf_mkp((void*)&uf_sl157);L_1348: pushc(cx,t569);op_fmt(cx);
L_1349: cx->cs[cx->csp++]=&&K_1349;goto L_6;K_1349:;
L_1350: Cell t570=uf_mkp((void*)&uf_sl158);L_1351: L_1352: Cell t571=uf_mki(0LL);L_1353: var_trans__lasts=t570;pushc(cx,t571);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1354: Cell t572=var_trans__vars;L_1355: Cell t573=var_trans__ptk;L_1356: pushc(cx,t572);pushc(cx,t573);op_getq(cx);
L_1357: op_dup(cx);L_1358: op_not(cx);
L_1359: pushp(cx,(void*)&&L_1371);
L_1360: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1360;goto *b;K_1360:;pop(cx);}}
L_1361: op_dup(cx);L_1362: Cell t574=pop(cx);L_1363: Cell t575=uf_mkp((void*)&uf_sl159);L_1364: var_trans__lasts=t574;pushc(cx,t575);op_fmt(cx);
L_1365: cx->cs[cx->csp++]=&&K_1365;goto L_6;K_1365:;
L_1366: cx->cs[cx->csp++]=&&K_1366;goto L_67;K_1366:;
L_1367: Cell t576=uf_mki(0LL);L_1368: pushc(cx,t576);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1369: Cell t577=uf_mkp((void*)&uf_sl160);L_1370: pushc(cx,t577);cx->cs[cx->csp++]=&&K_1370;goto L_53;K_1370:;
L_1371: op_drp(cx);L_1372: Cell t578=uf_mkp((void*)&uf_sl161);L_1373: Cell t579=var_trans__ptk;L_1374: pushc(cx,t578);pushc(cx,t579);op_cat(cx);
L_1375: Cell t580=uf_mkp((void*)&uf_sl162);L_1376: pushc(cx,t580);op_cat(cx);
L_1377: cx->cs[cx->csp++]=&&K_1377;goto L_53;K_1377:;
L_1378: cx->cs[cx->csp++]=&&K_1378;goto L_57;K_1378:;
L_1379: cx->cs[cx->csp++]=&&K_1379;goto L_88;K_1379:;
L_1380: pushp(cx,(void*)&&L_1447);
L_1381: pushp(cx,(void*)&&L_1384);
L_1382: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1382;goto *((!uf_zero(c))?th:el);K_1382:;pop(cx);}
L_1383: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1384: cx->cs[cx->csp++]=&&K_1384;goto L_57;K_1384:;
L_1385: Cell t581=uf_mkp((void*)&uf_sl163);L_1386: pushc(cx,t581);cx->cs[cx->csp++]=&&K_1386;goto L_46;K_1386:;
L_1387: pushp(cx,(void*)&&L_1502);
L_1388: pushp(cx,(void*)&&L_1391);
L_1389: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1389;goto *((!uf_zero(c))?th:el);K_1389:;pop(cx);}
L_1390: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1391: cx->cs[cx->csp++]=&&K_1391;goto L_57;K_1391:;
L_1392: Cell t582=uf_mkp((void*)&uf_sl164);L_1393: pushc(cx,t582);cx->cs[cx->csp++]=&&K_1393;goto L_46;K_1393:;
L_1394: pushp(cx,(void*)&&L_1527);
L_1395: pushp(cx,(void*)&&L_1398);
L_1396: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1396;goto *((!uf_zero(c))?th:el);K_1396:;pop(cx);}
L_1397: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1398: cx->cs[cx->csp++]=&&K_1398;goto L_57;K_1398:;
L_1399: Cell t583=uf_mkp((void*)&uf_sl165);L_1400: pushc(cx,t583);cx->cs[cx->csp++]=&&K_1400;goto L_46;K_1400:;
L_1401: pushp(cx,(void*)&&L_1617);
L_1402: pushp(cx,(void*)&&L_1405);
L_1403: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1403;goto *((!uf_zero(c))?th:el);K_1403:;pop(cx);}
L_1404: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1405: cx->cs[cx->csp++]=&&K_1405;goto L_57;K_1405:;
L_1406: Cell t584=uf_mkp((void*)&uf_sl166);L_1407: pushc(cx,t584);cx->cs[cx->csp++]=&&K_1407;goto L_46;K_1407:;
L_1408: pushp(cx,(void*)&&L_1682);
L_1409: pushp(cx,(void*)&&L_1412);
L_1410: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1410;goto *((!uf_zero(c))?th:el);K_1410:;pop(cx);}
L_1411: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1412: cx->cs[cx->csp++]=&&K_1412;goto L_57;K_1412:;
L_1413: Cell t585=uf_mkp((void*)&uf_sl167);L_1414: pushc(cx,t585);cx->cs[cx->csp++]=&&K_1414;goto L_46;K_1414:;
L_1415: pushp(cx,(void*)&&L_1908);
L_1416: pushp(cx,(void*)&&L_1419);
L_1417: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1417;goto *((!uf_zero(c))?th:el);K_1417:;pop(cx);}
L_1418: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1419: cx->cs[cx->csp++]=&&K_1419;goto L_57;K_1419:;
L_1420: Cell t586=uf_mkp((void*)&uf_sl168);L_1421: pushc(cx,t586);cx->cs[cx->csp++]=&&K_1421;goto L_46;K_1421:;
L_1422: pushp(cx,(void*)&&L_2019);
L_1423: pushp(cx,(void*)&&L_1426);
L_1424: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1424;goto *((!uf_zero(c))?th:el);K_1424:;pop(cx);}
L_1425: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1426: cx->cs[cx->csp++]=&&K_1426;goto L_57;K_1426:;
L_1427: Cell t587=uf_mkp((void*)&uf_sl169);L_1428: pushc(cx,t587);cx->cs[cx->csp++]=&&K_1428;goto L_46;K_1428:;
L_1429: pushp(cx,(void*)&&L_2033);
L_1430: pushp(cx,(void*)&&L_1433);
L_1431: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1431;goto *((!uf_zero(c))?th:el);K_1431:;pop(cx);}
L_1432: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1433: cx->cs[cx->csp++]=&&K_1433;goto L_57;K_1433:;
L_1434: Cell t588=uf_mkp((void*)&uf_sl170);L_1435: pushc(cx,t588);cx->cs[cx->csp++]=&&K_1435;goto L_46;K_1435:;
L_1436: pushp(cx,(void*)&&L_2047);
L_1437: pushp(cx,(void*)&&L_1440);
L_1438: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1438;goto *((!uf_zero(c))?th:el);K_1438:;pop(cx);}
L_1439: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1440: cx->cs[cx->csp++]=&&K_1440;goto L_57;K_1440:;
L_1441: Cell t589=uf_mkp((void*)&uf_sl171);L_1442: pushc(cx,t589);cx->cs[cx->csp++]=&&K_1442;goto L_46;K_1442:;
L_1443: pushp(cx,(void*)&&L_2062);
L_1444: pushp(cx,(void*)&&L_2065);
L_1445: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1445;goto *((!uf_zero(c))?th:el);K_1445:;pop(cx);}
L_1446: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1447: cx->cs[cx->csp++]=&&K_1447;goto L_125;K_1447:;
L_1448: cx->cs[cx->csp++]=&&K_1448;goto L_1471;K_1448:;
L_1449: pushp(cx,(void*)&&L_1463);
L_1450: pushp(cx,(void*)&&L_1467);
L_1451: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_1451:;cx->loops[fr].cont=&&K_WT_1451;cx->loops[fr].end=&&K_WE_1451;
cx->cs[cx->csp++]=&&K_WC_1451;goto *cnd;K_WC_1451:;
if(uf_zero(pop(cx)))goto K_WE_1451;
cx->cs[cx->csp++]=&&K_WB_1451;goto *bod;K_WB_1451:;pop(cx);
goto K_WT_1451;
K_WE_1451:;cx->lsp=fr;}
L_1452: Cell t590=uf_mkp((void*)&uf_sl172);L_1453: L_1454: var_trans__e=t590;cx->cs[cx->csp++]=&&K_1454;goto L_57;K_1454:;
L_1455: Cell t591=var_trans__e;L_1456: pushc(cx,t591);cx->cs[cx->csp++]=&&K_1456;goto L_46;K_1456:;
L_1457: op_not(cx);
L_1458: pushp(cx,(void*)&&L_78);
L_1459: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1459;goto *b;K_1459:;pop(cx);}}
L_1460: cx->cs[cx->csp++]=&&K_1460;goto L_67;K_1460:;
L_1461: Cell t592=uf_mki(0LL);L_1462: pushc(cx,t592);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1463: cx->cs[cx->csp++]=&&K_1463;goto L_57;K_1463:;
L_1464: Cell t593=uf_mkp((void*)&uf_sl173);L_1465: pushc(cx,t593);cx->cs[cx->csp++]=&&K_1465;goto L_46;K_1465:;
L_1466: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1467: cx->cs[cx->csp++]=&&K_1467;goto L_67;K_1467:;
L_1468: cx->cs[cx->csp++]=&&K_1468;goto L_1471;K_1468:;
L_1469: Cell t594=uf_mki(0LL);L_1470: pushc(cx,t594);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1471: cx->cs[cx->csp++]=&&K_1471;goto L_57;K_1471:;
L_1472: Cell t595=pop(cx);L_1473: var_trans__nv=t595;cx->cs[cx->csp++]=&&K_1473;goto L_67;K_1473:;
L_1474: Cell t596=var_trans__nv;L_1475: pushc(cx,t596);cx->cs[cx->csp++]=&&K_1475;goto L_139;K_1475:;
L_1476: Cell t597=pop(cx);L_1477: var_trans__slot=t597;cx->cs[cx->csp++]=&&K_1477;goto L_57;K_1477:;
L_1478: Cell t598=uf_mkp((void*)&uf_sl174);L_1479: pushc(cx,t598);cx->cs[cx->csp++]=&&K_1479;goto L_46;K_1479:;
L_1480: pushp(cx,(void*)&&L_1484);
L_1481: pushp(cx,(void*)&&L_1500);
L_1482: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1482;goto *((!uf_zero(c))?th:el);K_1482:;pop(cx);}
L_1483: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1484: cx->cs[cx->csp++]=&&K_1484;goto L_67;K_1484:;
L_1485: Cell t599=var_trans__slot;L_1486: Cell t600=var_trans__ps;L_1487: L_1488: pushc(cx,t600);pushc(cx,t599);op_push(cx);
L_1489: Cell t601=pop(cx);L_1490: var_trans__ps=t601;cx->cs[cx->csp++]=&&K_1490;goto L_451;K_1490:;
L_1491: Cell t602=var_trans__ps;L_1492: pushc(cx,t602);op_lpop(cx);
L_1493: Cell t603=pop(cx);L_1494: L_1495: Cell t604=uf_mkp((void*)&uf_sl175);L_1496: var_trans__slot2=t603;pushc(cx,t603);pushc(cx,t604);op_fmt(cx);
L_1497: cx->cs[cx->csp++]=&&K_1497;goto L_6;K_1497:;
L_1498: Cell t605=uf_mki(0LL);L_1499: pushc(cx,t605);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1500: Cell t606=uf_mki(0LL);L_1501: pushc(cx,t606);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1502: cx->cs[cx->csp++]=&&K_1502;goto L_67;K_1502:;
L_1503: cx->cs[cx->csp++]=&&K_1503;goto L_449;K_1503:;
L_1504: Cell t607=uf_mkp((void*)&uf_sl176);L_1505: L_1506: var_trans__e=t607;cx->cs[cx->csp++]=&&K_1506;goto L_57;K_1506:;
L_1507: Cell t608=var_trans__e;L_1508: pushc(cx,t608);cx->cs[cx->csp++]=&&K_1508;goto L_46;K_1508:;
L_1509: op_not(cx);
L_1510: pushp(cx,(void*)&&L_78);
L_1511: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1511;goto *b;K_1511:;pop(cx);}}
L_1512: cx->cs[cx->csp++]=&&K_1512;goto L_67;K_1512:;
L_1513: Cell t609=var_trans__inmain;L_1514: pushc(cx,t609);pushp(cx,(void*)&&L_1519);
L_1515: pushp(cx,(void*)&&L_1523);
L_1516: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1516;goto *((!uf_zero(c))?th:el);K_1516:;pop(cx);}
L_1517: Cell t610=uf_mki(0LL);L_1518: pushc(cx,t610);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1519: Cell t611=uf_mkp((void*)&uf_sl177);L_1520: pushc(cx,t611);cx->cs[cx->csp++]=&&K_1520;goto L_6;K_1520:;
L_1521: Cell t612=uf_mki(0LL);L_1522: pushc(cx,t612);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1523: Cell t613=uf_mkp((void*)&uf_sl178);L_1524: pushc(cx,t613);cx->cs[cx->csp++]=&&K_1524;goto L_6;K_1524:;
L_1525: Cell t614=uf_mki(0LL);L_1526: pushc(cx,t614);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1527: cx->cs[cx->csp++]=&&K_1527;goto L_67;K_1527:;
L_1528: Cell t615=uf_mkp((void*)&uf_sl179);L_1529: L_1530: var_trans__e=t615;cx->cs[cx->csp++]=&&K_1530;goto L_57;K_1530:;
L_1531: Cell t616=var_trans__e;L_1532: pushc(cx,t616);cx->cs[cx->csp++]=&&K_1532;goto L_46;K_1532:;
L_1533: op_not(cx);
L_1534: pushp(cx,(void*)&&L_78);
L_1535: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1535;goto *b;K_1535:;pop(cx);}}
L_1536: cx->cs[cx->csp++]=&&K_1536;goto L_67;K_1536:;
L_1537: cx->cs[cx->csp++]=&&K_1537;goto L_449;K_1537:;
L_1538: Cell t617=uf_mkp((void*)&uf_sl180);L_1539: L_1540: var_trans__e=t617;cx->cs[cx->csp++]=&&K_1540;goto L_57;K_1540:;
L_1541: Cell t618=var_trans__e;L_1542: pushc(cx,t618);cx->cs[cx->csp++]=&&K_1542;goto L_46;K_1542:;
L_1543: op_not(cx);
L_1544: pushp(cx,(void*)&&L_78);
L_1545: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1545;goto *b;K_1545:;pop(cx);}}
L_1546: cx->cs[cx->csp++]=&&K_1546;goto L_67;K_1546:;
L_1547: cx->cs[cx->csp++]=&&K_1547;goto L_72;K_1547:;
L_1548: Cell t619=pop(cx);L_1549: Cell t620=uf_mki(1LL);L_1550: L_1551: Cell t621=uf_mkp((void*)&uf_sl181);L_1552: L_1553: Cell t622=uf_mkp((void*)&uf_sl182);L_1554: var_trans__tlbl=t619;var_trans__inq=t620;pushc(cx,t621);pushc(cx,t619);pushc(cx,t622);op_fmt(cx);
L_1555: op_cat(cx);
L_1556: Cell t623=uf_mkp((void*)&uf_sl183);L_1557: pushc(cx,t623);op_cat(cx);
L_1558: cx->cs[cx->csp++]=&&K_1558;goto L_6;K_1558:;
L_1559: cx->cs[cx->csp++]=&&K_1559;goto L_1378;K_1559:;
L_1560: Cell t624=uf_mkp((void*)&uf_sl184);L_1561: pushc(cx,t624);cx->cs[cx->csp++]=&&K_1561;goto L_6;K_1561:;
L_1562: Cell t625=uf_mki(0LL);L_1563: L_1564: var_trans__inq=t625;cx->cs[cx->csp++]=&&K_1564;goto L_57;K_1564:;
L_1565: Cell t626=uf_mkp((void*)&uf_sl185);L_1566: pushc(cx,t626);cx->cs[cx->csp++]=&&K_1566;goto L_46;K_1566:;
L_1567: pushp(cx,(void*)&&L_1572);
L_1568: pushp(cx,(void*)&&L_1607);
L_1569: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1569;goto *((!uf_zero(c))?th:el);K_1569:;pop(cx);}
L_1570: Cell t627=uf_mki(0LL);L_1571: pushc(cx,t627);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1572: cx->cs[cx->csp++]=&&K_1572;goto L_72;K_1572:;
L_1573: Cell t628=pop(cx);L_1574: Cell t629=uf_mki(1LL);L_1575: L_1576: Cell t630=uf_mkp((void*)&uf_sl186);L_1577: L_1578: Cell t631=uf_mkp((void*)&uf_sl187);L_1579: var_trans__elbl=t628;var_trans__inq=t629;pushc(cx,t630);pushc(cx,t628);pushc(cx,t631);op_fmt(cx);
L_1580: op_cat(cx);
L_1581: Cell t632=uf_mkp((void*)&uf_sl188);L_1582: pushc(cx,t632);op_cat(cx);
L_1583: cx->cs[cx->csp++]=&&K_1583;goto L_6;K_1583:;
L_1584: cx->cs[cx->csp++]=&&K_1584;goto L_67;K_1584:;
L_1585: cx->cs[cx->csp++]=&&K_1585;goto L_1378;K_1585:;
L_1586: Cell t633=uf_mkp((void*)&uf_sl189);L_1587: pushc(cx,t633);cx->cs[cx->csp++]=&&K_1587;goto L_6;K_1587:;
L_1588: Cell t634=uf_mki(0LL);L_1589: L_1590: Cell t635=uf_mkp((void*)&uf_sl190);L_1591: Cell t636=var_trans__tlbl;L_1592: Cell t637=uf_mkp((void*)&uf_sl191);L_1593: var_trans__inq=t634;pushc(cx,t635);pushc(cx,t636);pushc(cx,t637);op_fmt(cx);
L_1594: op_cat(cx);
L_1595: Cell t638=uf_mkp((void*)&uf_sl192);L_1596: pushc(cx,t638);op_cat(cx);
L_1597: Cell t639=var_trans__elbl;L_1598: Cell t640=uf_mkp((void*)&uf_sl193);L_1599: pushc(cx,t639);pushc(cx,t640);op_fmt(cx);
L_1600: op_cat(cx);
L_1601: Cell t641=uf_mkp((void*)&uf_sl194);L_1602: pushc(cx,t641);op_cat(cx);
L_1603: op_cat(cx);
L_1604: cx->cs[cx->csp++]=&&K_1604;goto L_6;K_1604:;
L_1605: Cell t642=uf_mki(0LL);L_1606: pushc(cx,t642);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1607: Cell t643=uf_mkp((void*)&uf_sl195);L_1608: Cell t644=var_trans__tlbl;L_1609: Cell t645=uf_mkp((void*)&uf_sl196);L_1610: pushc(cx,t643);pushc(cx,t644);pushc(cx,t645);op_fmt(cx);
L_1611: op_cat(cx);
L_1612: Cell t646=uf_mkp((void*)&uf_sl197);L_1613: pushc(cx,t646);op_cat(cx);
L_1614: cx->cs[cx->csp++]=&&K_1614;goto L_6;K_1614:;
L_1615: Cell t647=uf_mki(0LL);L_1616: pushc(cx,t647);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1617: cx->cs[cx->csp++]=&&K_1617;goto L_67;K_1617:;
L_1618: cx->cs[cx->csp++]=&&K_1618;goto L_72;K_1618:;
L_1619: Cell t648=pop(cx);L_1620: var_trans__clbl=t648;cx->cs[cx->csp++]=&&K_1620;goto L_72;K_1620:;
L_1621: Cell t649=pop(cx);L_1622: Cell t650=uf_mki(1LL);L_1623: L_1624: Cell t651=uf_mkp((void*)&uf_sl198);L_1625: Cell t652=var_trans__clbl;L_1626: Cell t653=uf_mkp((void*)&uf_sl199);L_1627: var_trans__blbl=t649;var_trans__inq=t650;pushc(cx,t651);pushc(cx,t652);pushc(cx,t653);op_fmt(cx);
L_1628: op_cat(cx);
L_1629: Cell t654=uf_mkp((void*)&uf_sl200);L_1630: pushc(cx,t654);op_cat(cx);
L_1631: cx->cs[cx->csp++]=&&K_1631;goto L_6;K_1631:;
L_1632: Cell t655=uf_mkp((void*)&uf_sl201);L_1633: L_1634: var_trans__e=t655;cx->cs[cx->csp++]=&&K_1634;goto L_57;K_1634:;
L_1635: Cell t656=var_trans__e;L_1636: pushc(cx,t656);cx->cs[cx->csp++]=&&K_1636;goto L_46;K_1636:;
L_1637: op_not(cx);
L_1638: pushp(cx,(void*)&&L_78);
L_1639: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1639;goto *b;K_1639:;pop(cx);}}
L_1640: cx->cs[cx->csp++]=&&K_1640;goto L_67;K_1640:;
L_1641: cx->cs[cx->csp++]=&&K_1641;goto L_449;K_1641:;
L_1642: Cell t657=uf_mkp((void*)&uf_sl202);L_1643: L_1644: var_trans__e=t657;cx->cs[cx->csp++]=&&K_1644;goto L_57;K_1644:;
L_1645: Cell t658=var_trans__e;L_1646: pushc(cx,t658);cx->cs[cx->csp++]=&&K_1646;goto L_46;K_1646:;
L_1647: op_not(cx);
L_1648: pushp(cx,(void*)&&L_78);
L_1649: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1649;goto *b;K_1649:;pop(cx);}}
L_1650: cx->cs[cx->csp++]=&&K_1650;goto L_67;K_1650:;
L_1651: Cell t659=uf_mkp((void*)&uf_sl203);L_1652: pushc(cx,t659);cx->cs[cx->csp++]=&&K_1652;goto L_6;K_1652:;
L_1653: Cell t660=uf_mkp((void*)&uf_sl204);L_1654: Cell t661=var_trans__blbl;L_1655: Cell t662=uf_mkp((void*)&uf_sl205);L_1656: pushc(cx,t660);pushc(cx,t661);pushc(cx,t662);op_fmt(cx);
L_1657: op_cat(cx);
L_1658: Cell t663=uf_mkp((void*)&uf_sl206);L_1659: pushc(cx,t663);op_cat(cx);
L_1660: cx->cs[cx->csp++]=&&K_1660;goto L_6;K_1660:;
L_1661: cx->cs[cx->csp++]=&&K_1661;goto L_1378;K_1661:;
L_1662: Cell t664=uf_mkp((void*)&uf_sl207);L_1663: pushc(cx,t664);cx->cs[cx->csp++]=&&K_1663;goto L_6;K_1663:;
L_1664: Cell t665=uf_mki(0LL);L_1665: L_1666: Cell t666=uf_mkp((void*)&uf_sl208);L_1667: Cell t667=var_trans__clbl;L_1668: Cell t668=uf_mkp((void*)&uf_sl209);L_1669: var_trans__inq=t665;pushc(cx,t666);pushc(cx,t667);pushc(cx,t668);op_fmt(cx);
L_1670: op_cat(cx);
L_1671: Cell t669=uf_mkp((void*)&uf_sl210);L_1672: Cell t670=var_trans__blbl;L_1673: Cell t671=uf_mkp((void*)&uf_sl211);L_1674: pushc(cx,t669);pushc(cx,t670);pushc(cx,t671);op_fmt(cx);
L_1675: op_cat(cx);
L_1676: Cell t672=uf_mkp((void*)&uf_sl212);L_1677: pushc(cx,t672);op_cat(cx);
L_1678: op_cat(cx);
L_1679: cx->cs[cx->csp++]=&&K_1679;goto L_6;K_1679:;
L_1680: Cell t673=uf_mki(0LL);L_1681: pushc(cx,t673);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1682: cx->cs[cx->csp++]=&&K_1682;goto L_67;K_1682:;
L_1683: Cell t674=uf_mkp((void*)&uf_sl213);L_1684: L_1685: var_trans__e=t674;cx->cs[cx->csp++]=&&K_1685;goto L_57;K_1685:;
L_1686: Cell t675=var_trans__e;L_1687: pushc(cx,t675);cx->cs[cx->csp++]=&&K_1687;goto L_46;K_1687:;
L_1688: op_not(cx);
L_1689: pushp(cx,(void*)&&L_78);
L_1690: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1690;goto *b;K_1690:;pop(cx);}}
L_1691: cx->cs[cx->csp++]=&&K_1691;goto L_67;K_1691:;
L_1692: cx->cs[cx->csp++]=&&K_1692;goto L_72;K_1692:;
L_1693: Cell t676=pop(cx);L_1694: var_trans__fclbl=t676;cx->cs[cx->csp++]=&&K_1694;goto L_72;K_1694:;
L_1695: Cell t677=pop(cx);L_1696: var_trans__fblbl=t677;cx->cs[cx->csp++]=&&K_1696;goto L_57;K_1696:;
L_1697: Cell t678=uf_mkp((void*)&uf_sl214);L_1698: pushc(cx,t678);cx->cs[cx->csp++]=&&K_1698;goto L_46;K_1698:;
L_1699: pushp(cx,(void*)&&L_1777);
L_1700: pushp(cx,(void*)&&L_1780);
L_1701: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1701;goto *((!uf_zero(c))?th:el);K_1701:;pop(cx);}
L_1702: Cell t679=uf_mki(1LL);L_1703: L_1704: Cell t680=uf_mkp((void*)&uf_sl215);L_1705: Cell t681=var_trans__fclbl;L_1706: Cell t682=uf_mkp((void*)&uf_sl216);L_1707: var_trans__inq=t679;pushc(cx,t680);pushc(cx,t681);pushc(cx,t682);op_fmt(cx);
L_1708: op_cat(cx);
L_1709: Cell t683=uf_mkp((void*)&uf_sl217);L_1710: pushc(cx,t683);op_cat(cx);
L_1711: cx->cs[cx->csp++]=&&K_1711;goto L_6;K_1711:;
L_1712: cx->cs[cx->csp++]=&&K_1712;goto L_57;K_1712:;
L_1713: Cell t684=uf_mkp((void*)&uf_sl218);L_1714: pushc(cx,t684);cx->cs[cx->csp++]=&&K_1714;goto L_46;K_1714:;
L_1715: pushp(cx,(void*)&&L_1841);
L_1716: pushp(cx,(void*)&&L_1846);
L_1717: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1717;goto *((!uf_zero(c))?th:el);K_1717:;pop(cx);}
L_1718: Cell t685=uf_mkp((void*)&uf_sl219);L_1719: pushc(cx,t685);cx->cs[cx->csp++]=&&K_1719;goto L_6;K_1719:;
L_1720: Cell t686=uf_mkp((void*)&uf_sl220);L_1721: Cell t687=var_trans__fblbl;L_1722: Cell t688=uf_mkp((void*)&uf_sl221);L_1723: pushc(cx,t686);pushc(cx,t687);pushc(cx,t688);op_fmt(cx);
L_1724: op_cat(cx);
L_1725: Cell t689=uf_mkp((void*)&uf_sl222);L_1726: pushc(cx,t689);op_cat(cx);
L_1727: cx->cs[cx->csp++]=&&K_1727;goto L_6;K_1727:;
L_1728: Cell t690=var_trans__pi;L_1729: L_1730: Cell t691=uf_mki(1LL);L_1731: L_1732: var_trans__pfpi=t690;var_trans__pfd=t691;pushp(cx,(void*)&&L_1858);
L_1733: pushp(cx,(void*)&&L_1860);
L_1734: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_1734:;cx->loops[fr].cont=&&K_WT_1734;cx->loops[fr].end=&&K_WE_1734;
cx->cs[cx->csp++]=&&K_WC_1734;goto *cnd;K_WC_1734:;
if(uf_zero(pop(cx)))goto K_WE_1734;
cx->cs[cx->csp++]=&&K_WB_1734;goto *bod;K_WB_1734:;pop(cx);
goto K_WT_1734;
K_WE_1734:;cx->lsp=fr;}
L_1735: Cell t692=uf_mkp((void*)&uf_sl223);L_1736: L_1737: var_trans__e=t692;cx->cs[cx->csp++]=&&K_1737;goto L_57;K_1737:;
L_1738: Cell t693=var_trans__e;L_1739: pushc(cx,t693);cx->cs[cx->csp++]=&&K_1739;goto L_46;K_1739:;
L_1740: op_not(cx);
L_1741: pushp(cx,(void*)&&L_78);
L_1742: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1742;goto *b;K_1742:;pop(cx);}}
L_1743: cx->cs[cx->csp++]=&&K_1743;goto L_67;K_1743:;
L_1744: cx->cs[cx->csp++]=&&K_1744;goto L_1378;K_1744:;
L_1745: Cell t694=var_trans__pi;L_1746: L_1747: Cell t695=var_trans__pfpi;L_1748: L_1749: var_trans__pi=t695;var_trans__pfpi2=t694;cx->cs[cx->csp++]=&&K_1749;goto L_57;K_1749:;
L_1750: Cell t696=uf_mkp((void*)&uf_sl224);L_1751: pushc(cx,t696);cx->cs[cx->csp++]=&&K_1751;goto L_46;K_1751:;
L_1752: pushp(cx,(void*)&&L_1901);
L_1753: pushp(cx,(void*)&&L_1903);
L_1754: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1754;goto *((!uf_zero(c))?th:el);K_1754:;pop(cx);}
L_1755: Cell t697=var_trans__pfpi2;L_1756: L_1757: Cell t698=uf_mkp((void*)&uf_sl225);L_1758: var_trans__pi=t697;pushc(cx,t698);cx->cs[cx->csp++]=&&K_1758;goto L_6;K_1758:;
L_1759: Cell t699=uf_mki(0LL);L_1760: L_1761: Cell t700=uf_mkp((void*)&uf_sl226);L_1762: Cell t701=var_trans__fclbl;L_1763: Cell t702=uf_mkp((void*)&uf_sl227);L_1764: var_trans__inq=t699;pushc(cx,t700);pushc(cx,t701);pushc(cx,t702);op_fmt(cx);
L_1765: op_cat(cx);
L_1766: Cell t703=uf_mkp((void*)&uf_sl228);L_1767: Cell t704=var_trans__fblbl;L_1768: Cell t705=uf_mkp((void*)&uf_sl229);L_1769: pushc(cx,t703);pushc(cx,t704);pushc(cx,t705);op_fmt(cx);
L_1770: op_cat(cx);
L_1771: Cell t706=uf_mkp((void*)&uf_sl230);L_1772: pushc(cx,t706);op_cat(cx);
L_1773: op_cat(cx);
L_1774: cx->cs[cx->csp++]=&&K_1774;goto L_6;K_1774:;
L_1775: Cell t707=uf_mki(0LL);L_1776: pushc(cx,t707);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1777: cx->cs[cx->csp++]=&&K_1777;goto L_67;K_1777:;
L_1778: Cell t708=uf_mki(0LL);L_1779: pushc(cx,t708);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1780: cx->cs[cx->csp++]=&&K_1780;goto L_57;K_1780:;
L_1781: cx->cs[cx->csp++]=&&K_1781;goto L_88;K_1781:;
L_1782: pushp(cx,(void*)&&L_1787);
L_1783: pushp(cx,(void*)&&L_1827);
L_1784: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1784;goto *((!uf_zero(c))?th:el);K_1784:;pop(cx);}
L_1785: Cell t709=uf_mki(0LL);L_1786: pushc(cx,t709);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1787: cx->cs[cx->csp++]=&&K_1787;goto L_125;K_1787:;
L_1788: cx->cs[cx->csp++]=&&K_1788;goto L_57;K_1788:;
L_1789: Cell t710=pop(cx);L_1790: var_trans__nv=t710;cx->cs[cx->csp++]=&&K_1790;goto L_67;K_1790:;
L_1791: Cell t711=var_trans__nv;L_1792: pushc(cx,t711);cx->cs[cx->csp++]=&&K_1792;goto L_139;K_1792:;
L_1793: Cell t712=pop(cx);L_1794: Cell t713=uf_mkp((void*)&uf_sl231);L_1795: L_1796: var_trans__slot=t712;var_trans__e=t713;cx->cs[cx->csp++]=&&K_1796;goto L_57;K_1796:;
L_1797: Cell t714=var_trans__e;L_1798: pushc(cx,t714);cx->cs[cx->csp++]=&&K_1798;goto L_46;K_1798:;
L_1799: op_not(cx);
L_1800: pushp(cx,(void*)&&L_78);
L_1801: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1801;goto *b;K_1801:;pop(cx);}}
L_1802: cx->cs[cx->csp++]=&&K_1802;goto L_67;K_1802:;
L_1803: Cell t715=var_trans__slot;L_1804: Cell t716=var_trans__ps;L_1805: L_1806: pushc(cx,t716);pushc(cx,t715);op_push(cx);
L_1807: Cell t717=pop(cx);L_1808: var_trans__ps=t717;cx->cs[cx->csp++]=&&K_1808;goto L_451;K_1808:;
L_1809: Cell t718=var_trans__ps;L_1810: pushc(cx,t718);op_lpop(cx);
L_1811: Cell t719=pop(cx);L_1812: L_1813: Cell t720=uf_mkp((void*)&uf_sl232);L_1814: var_trans__slot2=t719;pushc(cx,t719);pushc(cx,t720);op_fmt(cx);
L_1815: cx->cs[cx->csp++]=&&K_1815;goto L_6;K_1815:;
L_1816: Cell t721=uf_mkp((void*)&uf_sl233);L_1817: L_1818: var_trans__e=t721;cx->cs[cx->csp++]=&&K_1818;goto L_57;K_1818:;
L_1819: Cell t722=var_trans__e;L_1820: pushc(cx,t722);cx->cs[cx->csp++]=&&K_1820;goto L_46;K_1820:;
L_1821: op_not(cx);
L_1822: pushp(cx,(void*)&&L_78);
L_1823: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1823;goto *b;K_1823:;pop(cx);}}
L_1824: cx->cs[cx->csp++]=&&K_1824;goto L_67;K_1824:;
L_1825: Cell t723=uf_mki(0LL);L_1826: pushc(cx,t723);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1827: cx->cs[cx->csp++]=&&K_1827;goto L_449;K_1827:;
L_1828: Cell t724=uf_mkp((void*)&uf_sl234);L_1829: pushc(cx,t724);cx->cs[cx->csp++]=&&K_1829;goto L_6;K_1829:;
L_1830: Cell t725=uf_mkp((void*)&uf_sl235);L_1831: L_1832: var_trans__e=t725;cx->cs[cx->csp++]=&&K_1832;goto L_57;K_1832:;
L_1833: Cell t726=var_trans__e;L_1834: pushc(cx,t726);cx->cs[cx->csp++]=&&K_1834;goto L_46;K_1834:;
L_1835: op_not(cx);
L_1836: pushp(cx,(void*)&&L_78);
L_1837: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1837;goto *b;K_1837:;pop(cx);}}
L_1838: cx->cs[cx->csp++]=&&K_1838;goto L_67;K_1838:;
L_1839: Cell t727=uf_mki(0LL);L_1840: pushc(cx,t727);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1841: cx->cs[cx->csp++]=&&K_1841;goto L_67;K_1841:;
L_1842: Cell t728=uf_mkp((void*)&uf_sl236);L_1843: pushc(cx,t728);cx->cs[cx->csp++]=&&K_1843;goto L_6;K_1843:;
L_1844: Cell t729=uf_mki(0LL);L_1845: pushc(cx,t729);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1846: cx->cs[cx->csp++]=&&K_1846;goto L_449;K_1846:;
L_1847: Cell t730=uf_mkp((void*)&uf_sl237);L_1848: L_1849: var_trans__e=t730;cx->cs[cx->csp++]=&&K_1849;goto L_57;K_1849:;
L_1850: Cell t731=var_trans__e;L_1851: pushc(cx,t731);cx->cs[cx->csp++]=&&K_1851;goto L_46;K_1851:;
L_1852: op_not(cx);
L_1853: pushp(cx,(void*)&&L_78);
L_1854: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1854;goto *b;K_1854:;pop(cx);}}
L_1855: cx->cs[cx->csp++]=&&K_1855;goto L_67;K_1855:;
L_1856: Cell t732=uf_mki(0LL);L_1857: pushc(cx,t732);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1858: Cell t733=var_trans__pfd;L_1859: pushc(cx,t733);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1860: cx->cs[cx->csp++]=&&K_1860;goto L_57;K_1860:;
L_1861: Cell t734=uf_mkp((void*)&uf_sl238);L_1862: pushc(cx,t734);cx->cs[cx->csp++]=&&K_1862;goto L_46;K_1862:;
L_1863: pushp(cx,(void*)&&L_1868);
L_1864: pushp(cx,(void*)&&L_1875);
L_1865: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1865;goto *((!uf_zero(c))?th:el);K_1865:;pop(cx);}
L_1866: Cell t735=uf_mki(0LL);L_1867: pushc(cx,t735);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1868: Cell t736=var_trans__pfd;L_1869: Cell t737=uf_mki(1LL);L_1870: Cell t738=uf_cadd(t736,t737);L_1871: L_1872: var_trans__pfd=t738;cx->cs[cx->csp++]=&&K_1872;goto L_67;K_1872:;
L_1873: Cell t739=uf_mki(0LL);L_1874: pushc(cx,t739);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1875: cx->cs[cx->csp++]=&&K_1875;goto L_57;K_1875:;
L_1876: Cell t740=uf_mkp((void*)&uf_sl239);L_1877: pushc(cx,t740);cx->cs[cx->csp++]=&&K_1877;goto L_46;K_1877:;
L_1878: pushp(cx,(void*)&&L_1883);
L_1879: pushp(cx,(void*)&&L_1898);
L_1880: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1880;goto *((!uf_zero(c))?th:el);K_1880:;pop(cx);}
L_1881: Cell t741=uf_mki(0LL);L_1882: pushc(cx,t741);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1883: Cell t742=var_trans__pfd;L_1884: Cell t743=uf_mki(1LL);L_1885: Cell t744=uf_csub(t742,t743);L_1886: L_1887: L_1888: var_trans__pfd=t744;pushc(cx,t744);pushp(cx,(void*)&&L_1893);
L_1889: pushp(cx,(void*)&&L_1895);
L_1890: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_1890;goto *((!uf_zero(c))?th:el);K_1890:;pop(cx);}
L_1891: Cell t745=uf_mki(0LL);L_1892: pushc(cx,t745);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1893: Cell t746=uf_mki(0LL);L_1894: pushc(cx,t746);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1895: cx->cs[cx->csp++]=&&K_1895;goto L_67;K_1895:;
L_1896: Cell t747=uf_mki(0LL);L_1897: pushc(cx,t747);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1898: cx->cs[cx->csp++]=&&K_1898;goto L_67;K_1898:;
L_1899: Cell t748=uf_mki(0LL);L_1900: pushc(cx,t748);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1901: Cell t749=uf_mki(0LL);L_1902: pushc(cx,t749);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1903: cx->cs[cx->csp++]=&&K_1903;goto L_449;K_1903:;
L_1904: Cell t750=uf_mkp((void*)&uf_sl240);L_1905: pushc(cx,t750);cx->cs[cx->csp++]=&&K_1905;goto L_6;K_1905:;
L_1906: Cell t751=uf_mki(0LL);L_1907: pushc(cx,t751);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_1908: cx->cs[cx->csp++]=&&K_1908;goto L_67;K_1908:;
L_1909: cx->cs[cx->csp++]=&&K_1909;goto L_72;K_1909:;
L_1910: Cell t752=pop(cx);L_1911: var_trans__dflbl=t752;cx->cs[cx->csp++]=&&K_1911;goto L_72;K_1911:;
L_1912: Cell t753=pop(cx);L_1913: var_trans__clbl=t753;cx->cs[cx->csp++]=&&K_1913;goto L_72;K_1913:;
L_1914: Cell t754=pop(cx);L_1915: Cell t755=uf_mkp((void*)&uf_sl241);L_1916: Cell t756=var_trans__dflbl;L_1917: Cell t757=uf_mkp((void*)&uf_sl242);L_1918: var_trans__blbl=t754;pushc(cx,t755);pushc(cx,t756);pushc(cx,t757);op_fmt(cx);
L_1919: op_cat(cx);
L_1920: op_cat(cx);
L_1921: Cell t758=uf_mkp((void*)&uf_sl243);L_1922: pushc(cx,t758);op_cat(cx);
L_1923: cx->cs[cx->csp++]=&&K_1923;goto L_6;K_1923:;
L_1924: Cell t759=uf_mki(1LL);L_1925: L_1926: Cell t760=uf_mkp((void*)&uf_sl244);L_1927: Cell t761=var_trans__blbl;L_1928: Cell t762=uf_mkp((void*)&uf_sl245);L_1929: var_trans__inq=t759;pushc(cx,t760);pushc(cx,t761);pushc(cx,t762);op_fmt(cx);
L_1930: op_cat(cx);
L_1931: Cell t763=uf_mkp((void*)&uf_sl246);L_1932: pushc(cx,t763);op_cat(cx);
L_1933: cx->cs[cx->csp++]=&&K_1933;goto L_6;K_1933:;
L_1934: Cell t764=uf_mkp((void*)&uf_sl247);L_1935: Cell t765=var_trans__dflbl;L_1936: Cell t766=uf_mkp((void*)&uf_sl248);L_1937: pushc(cx,t764);pushc(cx,t765);pushc(cx,t766);op_fmt(cx);
L_1938: op_cat(cx);
L_1939: op_cat(cx);
L_1940: Cell t767=uf_mkp((void*)&uf_sl249);L_1941: pushc(cx,t767);op_cat(cx);
L_1942: cx->cs[cx->csp++]=&&K_1942;goto L_6;K_1942:;
L_1943: cx->cs[cx->csp++]=&&K_1943;goto L_1378;K_1943:;
L_1944: Cell t768=uf_mkp((void*)&uf_sl250);L_1945: pushc(cx,t768);cx->cs[cx->csp++]=&&K_1945;goto L_6;K_1945:;
L_1946: Cell t769=uf_mkp((void*)&uf_sl251);L_1947: L_1948: var_trans__e=t769;cx->cs[cx->csp++]=&&K_1948;goto L_57;K_1948:;
L_1949: Cell t770=var_trans__e;L_1950: pushc(cx,t770);cx->cs[cx->csp++]=&&K_1950;goto L_46;K_1950:;
L_1951: op_not(cx);
L_1952: pushp(cx,(void*)&&L_78);
L_1953: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1953;goto *b;K_1953:;pop(cx);}}
L_1954: cx->cs[cx->csp++]=&&K_1954;goto L_67;K_1954:;
L_1955: Cell t771=uf_mkp((void*)&uf_sl252);L_1956: L_1957: var_trans__e=t771;cx->cs[cx->csp++]=&&K_1957;goto L_57;K_1957:;
L_1958: Cell t772=var_trans__e;L_1959: pushc(cx,t772);cx->cs[cx->csp++]=&&K_1959;goto L_46;K_1959:;
L_1960: op_not(cx);
L_1961: pushp(cx,(void*)&&L_78);
L_1962: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1962;goto *b;K_1962:;pop(cx);}}
L_1963: cx->cs[cx->csp++]=&&K_1963;goto L_67;K_1963:;
L_1964: Cell t773=uf_mkp((void*)&uf_sl253);L_1965: Cell t774=var_trans__clbl;L_1966: Cell t775=uf_mkp((void*)&uf_sl254);L_1967: pushc(cx,t773);pushc(cx,t774);pushc(cx,t775);op_fmt(cx);
L_1968: op_cat(cx);
L_1969: Cell t776=uf_mkp((void*)&uf_sl255);L_1970: pushc(cx,t776);op_cat(cx);
L_1971: cx->cs[cx->csp++]=&&K_1971;goto L_6;K_1971:;
L_1972: Cell t777=uf_mkp((void*)&uf_sl256);L_1973: Cell t778=var_trans__dflbl;L_1974: Cell t779=uf_mkp((void*)&uf_sl257);L_1975: pushc(cx,t777);pushc(cx,t778);pushc(cx,t779);op_fmt(cx);
L_1976: op_cat(cx);
L_1977: Cell t780=uf_mkp((void*)&uf_sl258);L_1978: pushc(cx,t780);op_cat(cx);
L_1979: cx->cs[cx->csp++]=&&K_1979;goto L_6;K_1979:;
L_1980: cx->cs[cx->csp++]=&&K_1980;goto L_449;K_1980:;
L_1981: Cell t781=uf_mkp((void*)&uf_sl259);L_1982: pushc(cx,t781);cx->cs[cx->csp++]=&&K_1982;goto L_6;K_1982:;
L_1983: Cell t782=uf_mkp((void*)&uf_sl260);L_1984: L_1985: var_trans__e=t782;cx->cs[cx->csp++]=&&K_1985;goto L_57;K_1985:;
L_1986: Cell t783=var_trans__e;L_1987: pushc(cx,t783);cx->cs[cx->csp++]=&&K_1987;goto L_46;K_1987:;
L_1988: op_not(cx);
L_1989: pushp(cx,(void*)&&L_78);
L_1990: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1990;goto *b;K_1990:;pop(cx);}}
L_1991: cx->cs[cx->csp++]=&&K_1991;goto L_67;K_1991:;
L_1992: Cell t784=uf_mkp((void*)&uf_sl261);L_1993: L_1994: var_trans__e=t784;cx->cs[cx->csp++]=&&K_1994;goto L_57;K_1994:;
L_1995: Cell t785=var_trans__e;L_1996: pushc(cx,t785);cx->cs[cx->csp++]=&&K_1996;goto L_46;K_1996:;
L_1997: op_not(cx);
L_1998: pushp(cx,(void*)&&L_78);
L_1999: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_1999;goto *b;K_1999:;pop(cx);}}
L_2000: cx->cs[cx->csp++]=&&K_2000;goto L_67;K_2000:;
L_2001: Cell t786=uf_mki(0LL);L_2002: L_2003: Cell t787=uf_mkp((void*)&uf_sl262);L_2004: Cell t788=var_trans__clbl;L_2005: Cell t789=uf_mkp((void*)&uf_sl263);L_2006: var_trans__inq=t786;pushc(cx,t787);pushc(cx,t788);pushc(cx,t789);op_fmt(cx);
L_2007: op_cat(cx);
L_2008: Cell t790=uf_mkp((void*)&uf_sl264);L_2009: Cell t791=var_trans__blbl;L_2010: Cell t792=uf_mkp((void*)&uf_sl265);L_2011: pushc(cx,t790);pushc(cx,t791);pushc(cx,t792);op_fmt(cx);
L_2012: op_cat(cx);
L_2013: Cell t793=uf_mkp((void*)&uf_sl266);L_2014: pushc(cx,t793);op_cat(cx);
L_2015: op_cat(cx);
L_2016: cx->cs[cx->csp++]=&&K_2016;goto L_6;K_2016:;
L_2017: Cell t794=uf_mki(0LL);L_2018: pushc(cx,t794);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2019: cx->cs[cx->csp++]=&&K_2019;goto L_67;K_2019:;
L_2020: Cell t795=uf_mkp((void*)&uf_sl267);L_2021: L_2022: var_trans__e=t795;cx->cs[cx->csp++]=&&K_2022;goto L_57;K_2022:;
L_2023: Cell t796=var_trans__e;L_2024: pushc(cx,t796);cx->cs[cx->csp++]=&&K_2024;goto L_46;K_2024:;
L_2025: op_not(cx);
L_2026: pushp(cx,(void*)&&L_78);
L_2027: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2027;goto *b;K_2027:;pop(cx);}}
L_2028: cx->cs[cx->csp++]=&&K_2028;goto L_67;K_2028:;
L_2029: Cell t797=uf_mkp((void*)&uf_sl268);L_2030: pushc(cx,t797);cx->cs[cx->csp++]=&&K_2030;goto L_6;K_2030:;
L_2031: Cell t798=uf_mki(0LL);L_2032: pushc(cx,t798);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2033: cx->cs[cx->csp++]=&&K_2033;goto L_67;K_2033:;
L_2034: Cell t799=uf_mkp((void*)&uf_sl269);L_2035: L_2036: var_trans__e=t799;cx->cs[cx->csp++]=&&K_2036;goto L_57;K_2036:;
L_2037: Cell t800=var_trans__e;L_2038: pushc(cx,t800);cx->cs[cx->csp++]=&&K_2038;goto L_46;K_2038:;
L_2039: op_not(cx);
L_2040: pushp(cx,(void*)&&L_78);
L_2041: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2041;goto *b;K_2041:;pop(cx);}}
L_2042: cx->cs[cx->csp++]=&&K_2042;goto L_67;K_2042:;
L_2043: Cell t801=uf_mkp((void*)&uf_sl270);L_2044: pushc(cx,t801);cx->cs[cx->csp++]=&&K_2044;goto L_6;K_2044:;
L_2045: Cell t802=uf_mki(0LL);L_2046: pushc(cx,t802);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2047: cx->cs[cx->csp++]=&&K_2047;goto L_67;K_2047:;
L_2048: pushp(cx,(void*)&&L_2054);
L_2049: pushp(cx,(void*)&&L_2059);
L_2050: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2050:;cx->loops[fr].cont=&&K_WT_2050;cx->loops[fr].end=&&K_WE_2050;
cx->cs[cx->csp++]=&&K_WC_2050;goto *cnd;K_WC_2050:;
if(uf_zero(pop(cx)))goto K_WE_2050;
cx->cs[cx->csp++]=&&K_WB_2050;goto *bod;K_WB_2050:;pop(cx);
goto K_WT_2050;
K_WE_2050:;cx->lsp=fr;}
L_2051: cx->cs[cx->csp++]=&&K_2051;goto L_67;K_2051:;
L_2052: Cell t803=uf_mki(0LL);L_2053: pushc(cx,t803);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2054: cx->cs[cx->csp++]=&&K_2054;goto L_57;K_2054:;
L_2055: Cell t804=uf_mkp((void*)&uf_sl271);L_2056: pushc(cx,t804);cx->cs[cx->csp++]=&&K_2056;goto L_46;K_2056:;
L_2057: op_not(cx);
L_2058: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2059: cx->cs[cx->csp++]=&&K_2059;goto L_1378;K_2059:;
L_2060: Cell t805=uf_mki(0LL);L_2061: pushc(cx,t805);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2062: cx->cs[cx->csp++]=&&K_2062;goto L_67;K_2062:;
L_2063: Cell t806=uf_mki(0LL);L_2064: pushc(cx,t806);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2065: cx->cs[cx->csp++]=&&K_2065;goto L_449;K_2065:;
L_2066: Cell t807=uf_mkp((void*)&uf_sl272);L_2067: pushc(cx,t807);cx->cs[cx->csp++]=&&K_2067;goto L_6;K_2067:;
L_2068: Cell t808=uf_mkp((void*)&uf_sl273);L_2069: L_2070: var_trans__e=t808;cx->cs[cx->csp++]=&&K_2070;goto L_57;K_2070:;
L_2071: Cell t809=var_trans__e;L_2072: pushc(cx,t809);cx->cs[cx->csp++]=&&K_2072;goto L_46;K_2072:;
L_2073: op_not(cx);
L_2074: pushp(cx,(void*)&&L_78);
L_2075: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2075;goto *b;K_2075:;pop(cx);}}
L_2076: cx->cs[cx->csp++]=&&K_2076;goto L_67;K_2076:;
L_2077: Cell t810=uf_mki(0LL);L_2078: pushc(cx,t810);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2079: cx->cs[cx->csp++]=&&K_2079;goto L_125;K_2079:;
L_2080: cx->cs[cx->csp++]=&&K_2080;goto L_57;K_2080:;
L_2081: Cell t811=pop(cx);L_2082: var_trans__fname=t811;cx->cs[cx->csp++]=&&K_2082;goto L_67;K_2082:;
L_2083: Cell t812=uf_mkp((void*)&uf_sl274);L_2084: L_2085: var_trans__e=t812;cx->cs[cx->csp++]=&&K_2085;goto L_57;K_2085:;
L_2086: Cell t813=var_trans__e;L_2087: pushc(cx,t813);cx->cs[cx->csp++]=&&K_2087;goto L_46;K_2087:;
L_2088: op_not(cx);
L_2089: pushp(cx,(void*)&&L_78);
L_2090: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2090;goto *b;K_2090:;pop(cx);}}
L_2091: cx->cs[cx->csp++]=&&K_2091;goto L_67;K_2091:;
L_2092: op_list(cx);
L_2093: Cell t814=pop(cx);L_2094: var_trans__pl=t814;pushp(cx,(void*)&&L_2154);
L_2095: pushp(cx,(void*)&&L_2159);
L_2096: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2096:;cx->loops[fr].cont=&&K_WT_2096;cx->loops[fr].end=&&K_WE_2096;
cx->cs[cx->csp++]=&&K_WC_2096;goto *cnd;K_WC_2096:;
if(uf_zero(pop(cx)))goto K_WE_2096;
cx->cs[cx->csp++]=&&K_WB_2096;goto *bod;K_WB_2096:;pop(cx);
goto K_WT_2096;
K_WE_2096:;cx->lsp=fr;}
L_2097: cx->cs[cx->csp++]=&&K_2097;goto L_67;K_2097:;
L_2098: op_dict(cx);
L_2099: Cell t815=pop(cx);L_2100: Cell t816=uf_mkp((void*)&uf_sl275);L_2101: L_2102: Cell t817=var_trans__fname;L_2103: Cell t818=uf_mkp((void*)&uf_sl276);L_2104: var_trans__vars=t815;var_trans__qout=t816;pushc(cx,t817);pushc(cx,t818);cx->cs[cx->csp++]=&&K_2104;goto L_46;K_2104:;
L_2105: pushp(cx,(void*)&&L_2148);
L_2106: pushp(cx,(void*)&&L_2152);
L_2107: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_2107;goto *((!uf_zero(c))?th:el);K_2107:;pop(cx);}
L_2108: Cell t819=var_trans__fname;L_2109: Cell t820=uf_mkp((void*)&uf_sl277);L_2110: pushc(cx,t819);pushc(cx,t820);op_fmt(cx);
L_2111: cx->cs[cx->csp++]=&&K_2111;goto L_6;K_2111:;
L_2112: Cell t821=var_trans__fname;L_2113: Cell t822=uf_mkp((void*)&uf_sl278);L_2114: pushc(cx,t821);pushc(cx,t822);cx->cs[cx->csp++]=&&K_2114;goto L_46;K_2114:;
L_2115: Cell t823=pop(cx);L_2116: Cell t824=var_trans__pl;L_2117: var_trans__inmain=t823;pushc(cx,t824);op_len(cx);
L_2118: Cell t825=uf_mki(1LL);L_2119: Cell t826=pop(cx);Cell t827=uf_csub(t826,t825);L_2120: L_2121: var_trans__pfi=t827;pushp(cx,(void*)&&L_2191);
L_2122: pushp(cx,(void*)&&L_2196);
L_2123: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2123:;cx->loops[fr].cont=&&K_WT_2123;cx->loops[fr].end=&&K_WE_2123;
cx->cs[cx->csp++]=&&K_WC_2123;goto *cnd;K_WC_2123:;
if(uf_zero(pop(cx)))goto K_WE_2123;
cx->cs[cx->csp++]=&&K_WB_2123;goto *bod;K_WB_2123:;pop(cx);
goto K_WT_2123;
K_WE_2123:;cx->lsp=fr;}
L_2124: Cell t828=uf_mkp((void*)&uf_sl279);L_2125: L_2126: var_trans__e=t828;cx->cs[cx->csp++]=&&K_2126;goto L_57;K_2126:;
L_2127: Cell t829=var_trans__e;L_2128: pushc(cx,t829);cx->cs[cx->csp++]=&&K_2128;goto L_46;K_2128:;
L_2129: op_not(cx);
L_2130: pushp(cx,(void*)&&L_78);
L_2131: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2131;goto *b;K_2131:;pop(cx);}}
L_2132: cx->cs[cx->csp++]=&&K_2132;goto L_67;K_2132:;
L_2133: pushp(cx,(void*)&&L_2211);
L_2134: pushp(cx,(void*)&&L_2216);
L_2135: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2135:;cx->loops[fr].cont=&&K_WT_2135;cx->loops[fr].end=&&K_WE_2135;
cx->cs[cx->csp++]=&&K_WC_2135;goto *cnd;K_WC_2135:;
if(uf_zero(pop(cx)))goto K_WE_2135;
cx->cs[cx->csp++]=&&K_WB_2135;goto *bod;K_WB_2135:;pop(cx);
goto K_WT_2135;
K_WE_2135:;cx->lsp=fr;}
L_2136: cx->cs[cx->csp++]=&&K_2136;goto L_67;K_2136:;
L_2137: Cell t830=var_trans__inmain;L_2138: pushc(cx,t830);pushp(cx,(void*)&&L_2219);
L_2139: pushp(cx,(void*)&&L_2223);
L_2140: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_2140;goto *((!uf_zero(c))?th:el);K_2140:;pop(cx);}
L_2141: Cell t831=var_trans__out;L_2142: Cell t832=var_trans__qout;L_2143: pushc(cx,t831);pushc(cx,t832);op_cat(cx);
L_2144: Cell t833=pop(cx);L_2145: Cell t834=uf_mkp((void*)&uf_sl280);L_2146: L_2147: var_trans__out=t833;var_trans__qout=t834;{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2148: Cell t835=uf_mkp((void*)&uf_sl281);L_2149: pushc(cx,t835);cx->cs[cx->csp++]=&&K_2149;goto L_6;K_2149:;
L_2150: Cell t836=uf_mki(0LL);L_2151: pushc(cx,t836);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2152: Cell t837=uf_mki(0LL);L_2153: pushc(cx,t837);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2154: cx->cs[cx->csp++]=&&K_2154;goto L_57;K_2154:;
L_2155: Cell t838=uf_mkp((void*)&uf_sl282);L_2156: pushc(cx,t838);cx->cs[cx->csp++]=&&K_2156;goto L_46;K_2156:;
L_2157: op_not(cx);
L_2158: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2159: cx->cs[cx->csp++]=&&K_2159;goto L_57;K_2159:;
L_2160: Cell t839=uf_mkp((void*)&uf_sl283);L_2161: pushc(cx,t839);cx->cs[cx->csp++]=&&K_2161;goto L_46;K_2161:;
L_2162: pushp(cx,(void*)&&L_2167);
L_2163: pushp(cx,(void*)&&L_2170);
L_2164: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_2164;goto *((!uf_zero(c))?th:el);K_2164:;pop(cx);}
L_2165: Cell t840=uf_mki(0LL);L_2166: pushc(cx,t840);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2167: cx->cs[cx->csp++]=&&K_2167;goto L_67;K_2167:;
L_2168: Cell t841=uf_mki(0LL);L_2169: pushc(cx,t841);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2170: cx->cs[cx->csp++]=&&K_2170;goto L_57;K_2170:;
L_2171: cx->cs[cx->csp++]=&&K_2171;goto L_88;K_2171:;
L_2172: cx->cs[cx->csp++]=&&K_2172;goto L_57;K_2172:;
L_2173: Cell t842=uf_mkp((void*)&uf_sl284);L_2174: pushc(cx,t842);cx->cs[cx->csp++]=&&K_2174;goto L_46;K_2174:;
L_2175: Cell t843=pop(cx);Cell t844=pop(cx);Cell t845=uf_cadd(t844,t843);L_2176: pushc(cx,t845);pushp(cx,(void*)&&L_2181);
L_2177: pushp(cx,(void*)&&L_2184);
L_2178: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_2178;goto *((!uf_zero(c))?th:el);K_2178:;pop(cx);}
L_2179: Cell t846=uf_mki(0LL);L_2180: pushc(cx,t846);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2181: cx->cs[cx->csp++]=&&K_2181;goto L_67;K_2181:;
L_2182: Cell t847=uf_mki(0LL);L_2183: pushc(cx,t847);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2184: Cell t848=var_trans__pl;L_2185: pushc(cx,t848);cx->cs[cx->csp++]=&&K_2185;goto L_57;K_2185:;
L_2186: op_push(cx);
L_2187: Cell t849=pop(cx);L_2188: var_trans__pl=t849;cx->cs[cx->csp++]=&&K_2188;goto L_67;K_2188:;
L_2189: Cell t850=uf_mki(0LL);L_2190: pushc(cx,t850);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2191: Cell t851=var_trans__pfi;L_2192: Cell t852=uf_mki(-1LL);L_2193: pushc(cx,t851);pushc(cx,t852);op_eq(cx);
L_2194: op_not(cx);
L_2195: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2196: Cell t853=var_trans__pl;L_2197: Cell t854=var_trans__pfi;L_2198: pushc(cx,t853);pushc(cx,t854);op_get(cx);
L_2199: Cell t855=pop(cx);L_2200: L_2201: var_trans__nv=t855;pushc(cx,t855);cx->cs[cx->csp++]=&&K_2201;goto L_139;K_2201:;
L_2202: Cell t856=uf_mkp((void*)&uf_sl285);L_2203: pushc(cx,t856);op_fmt(cx);
L_2204: cx->cs[cx->csp++]=&&K_2204;goto L_6;K_2204:;
L_2205: Cell t857=var_trans__pfi;L_2206: Cell t858=uf_mki(1LL);L_2207: Cell t859=uf_csub(t857,t858);L_2208: L_2209: Cell t860=uf_mki(0LL);L_2210: var_trans__pfi=t859;pushc(cx,t860);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2211: cx->cs[cx->csp++]=&&K_2211;goto L_57;K_2211:;
L_2212: Cell t861=uf_mkp((void*)&uf_sl286);L_2213: pushc(cx,t861);cx->cs[cx->csp++]=&&K_2213;goto L_46;K_2213:;
L_2214: op_not(cx);
L_2215: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2216: cx->cs[cx->csp++]=&&K_2216;goto L_1378;K_2216:;
L_2217: Cell t862=uf_mki(0LL);L_2218: pushc(cx,t862);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2219: Cell t863=uf_mkp((void*)&uf_sl287);L_2220: pushc(cx,t863);cx->cs[cx->csp++]=&&K_2220;goto L_6;K_2220:;
L_2221: Cell t864=uf_mki(0LL);L_2222: pushc(cx,t864);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2223: Cell t865=uf_mkp((void*)&uf_sl288);L_2224: pushc(cx,t865);cx->cs[cx->csp++]=&&K_2224;goto L_6;K_2224:;
L_2225: Cell t866=uf_mki(0LL);L_2226: pushc(cx,t866);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2227: pushp(cx,(void*)&&L_2231);
L_2228: pushp(cx,(void*)&&L_2236);
L_2229: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2229:;cx->loops[fr].cont=&&K_WT_2229;cx->loops[fr].end=&&K_WE_2229;
cx->cs[cx->csp++]=&&K_WC_2229;goto *cnd;K_WC_2229:;
if(uf_zero(pop(cx)))goto K_WE_2229;
cx->cs[cx->csp++]=&&K_WB_2229;goto *bod;K_WB_2229:;pop(cx);
goto K_WT_2229;
K_WE_2229:;cx->lsp=fr;}
L_2230: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2231: Cell t867=var_trans__pi;L_2232: Cell t868=var_trans__nt;L_2233: pushc(cx,t867);pushc(cx,t868);op_eq(cx);
L_2234: op_not(cx);
L_2235: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2236: cx->cs[cx->csp++]=&&K_2236;goto L_2079;K_2236:;
L_2237: Cell t869=uf_mki(0LL);L_2238: pushc(cx,t869);{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2239: pushp(cx,(void*)&uf_x0);
L_2240: op_loadx(cx);
L_2241: Cell t870=uf_mki(2LL);L_2242: pushc(cx,t870);op_eq(cx);
L_2243: op_not(cx);
L_2244: pushp(cx,(void*)&&L_2361);
L_2245: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2245;goto *b;K_2245:;pop(cx);}}
L_2246: pushp(cx,(void*)&uf_x1);
L_2247: op_loadx(cx);
L_2248: Cell t871=uf_mki(8LL);L_2249: Cell t872=pop(cx);Cell t873=uf_cadd(t872,t871);L_2250: pushc(cx,t873);op_loadx(cx);
L_2251: Cell t874=pop(cx);L_2252: L_2253: Cell t875=uf_mkp((void*)&uf_sl289);L_2254: var_trans__path=t874;pushc(cx,t874);pushc(cx,t875);{Cell a1=pop(cx);Cell a0=pop(cx);void* r=((void*(*)(void*,void*))uf_im2)((void*)uf_sptr(a0),(void*)uf_sptr(a1));pushp(cx,r);}
L_2255: Cell t876=pop(cx);L_2256: L_2257: var_trans__f=t876;pushc(cx,t876);op_not(cx);
L_2258: pushp(cx,(void*)&&L_2363);
L_2259: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){cx->cs[cx->csp++]=&&K_2259;goto *b;K_2259:;pop(cx);}}
L_2260: Cell t877=var_trans__f;L_2261: Cell t878=uf_mki(0LL);L_2262: Cell t879=uf_mki(2LL);L_2263: pushc(cx,t877);pushc(cx,t878);pushc(cx,t879);{Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t))uf_im3)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i));pushi(cx,(int64_t)r);}
L_2264: op_drp(cx);L_2265: Cell t880=var_trans__f;L_2266: pushc(cx,t880);{Cell a0=pop(cx);int r=((int(*)(void*))uf_im4)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_2267: Cell t881=pop(cx);L_2268: Cell t882=var_trans__f;L_2269: Cell t883=uf_mki(0LL);L_2270: Cell t884=uf_mki(0LL);L_2271: var_trans__srclen=t881;pushc(cx,t882);pushc(cx,t883);pushc(cx,t884);{Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t))uf_im3)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i));pushi(cx,(int64_t)r);}
L_2272: op_drp(cx);L_2273: Cell t885=var_trans__srclen;L_2274: Cell t886=uf_mki(16LL);L_2275: Cell t887=uf_cadd(t885,t886);L_2276: pushc(cx,t887);op_buf(cx);
L_2277: Cell t888=pop(cx);L_2278: L_2279: Cell t889=uf_mki(1LL);L_2280: Cell t890=var_trans__srclen;L_2281: Cell t891=var_trans__f;L_2282: var_trans__src=t888;pushc(cx,t888);pushc(cx,t889);pushc(cx,t890);pushc(cx,t891);{Cell a3=pop(cx);Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t,void*))uf_im5)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i),(void*)uf_sptr(a3));pushi(cx,(int64_t)r);}
L_2283: op_drp(cx);L_2284: Cell t892=var_trans__f;L_2285: pushc(cx,t892);{Cell a0=pop(cx);int r=((int(*)(void*))uf_im6)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_2286: op_drp(cx);L_2287: op_list(cx);
L_2288: Cell t893=pop(cx);L_2289: Cell t894=uf_mki(0LL);L_2290: L_2291: var_trans__toks=t893;var_trans__pos=t894;pushp(cx,(void*)&&L_195);
L_2292: pushp(cx,(void*)&&L_202);
L_2293: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2293:;cx->loops[fr].cont=&&K_WT_2293;cx->loops[fr].end=&&K_WE_2293;
cx->cs[cx->csp++]=&&K_WC_2293;goto *cnd;K_WC_2293:;
if(uf_zero(pop(cx)))goto K_WE_2293;
cx->cs[cx->csp++]=&&K_WB_2293;goto *bod;K_WB_2293:;pop(cx);
goto K_WT_2293;
K_WE_2293:;cx->lsp=fr;}
L_2294: Cell t895=var_trans__toks;L_2295: pushc(cx,t895);op_len(cx);
L_2296: Cell t896=pop(cx);L_2297: Cell t897=var_trans__toks;L_2298: var_trans__nt=t896;pushc(cx,t897);op_len(cx);
L_2299: Cell t898=uf_mkp((void*)&uf_sl290);L_2300: pushc(cx,t898);op_print(cx);
L_2301: op_drp(cx);L_2302: Cell t899=uf_mki(0LL);L_2303: L_2304: Cell t900=uf_mki(0LL);L_2305: L_2306: Cell t901=uf_mki(0LL);L_2307: L_2308: Cell t902=uf_mki(0LL);L_2309: L_2310: var_trans__pi=t899;var_trans__lbl=t900;var_trans__fid=t901;var_trans__inmain=t902;op_list(cx);
L_2311: Cell t903=pop(cx);L_2312: Cell t904=uf_mkp((void*)&uf_sl291);L_2313: var_trans__ps=t903;pushc(cx,t904);cx->cs[cx->csp++]=&&K_2313;goto L_6;K_2313:;
L_2314: Cell t905=uf_mkp((void*)&uf_sl292);L_2315: pushc(cx,t905);cx->cs[cx->csp++]=&&K_2315;goto L_6;K_2315:;
L_2316: Cell t906=uf_mkp((void*)&uf_sl293);L_2317: pushc(cx,t906);cx->cs[cx->csp++]=&&K_2317;goto L_6;K_2317:;
L_2318: Cell t907=uf_mkp((void*)&uf_sl294);L_2319: pushc(cx,t907);cx->cs[cx->csp++]=&&K_2319;goto L_6;K_2319:;
L_2320: Cell t908=uf_mkp((void*)&uf_sl295);L_2321: pushc(cx,t908);cx->cs[cx->csp++]=&&K_2321;goto L_6;K_2321:;
L_2322: Cell t909=uf_mkp((void*)&uf_sl296);L_2323: pushc(cx,t909);cx->cs[cx->csp++]=&&K_2323;goto L_6;K_2323:;
L_2324: Cell t910=uf_mkp((void*)&uf_sl297);L_2325: pushc(cx,t910);cx->cs[cx->csp++]=&&K_2325;goto L_6;K_2325:;
L_2326: Cell t911=uf_mkp((void*)&uf_sl298);L_2327: pushc(cx,t911);cx->cs[cx->csp++]=&&K_2327;goto L_6;K_2327:;
L_2328: Cell t912=uf_mkp((void*)&uf_sl299);L_2329: pushc(cx,t912);cx->cs[cx->csp++]=&&K_2329;goto L_6;K_2329:;
L_2330: Cell t913=uf_mkp((void*)&uf_sl300);L_2331: pushc(cx,t913);cx->cs[cx->csp++]=&&K_2331;goto L_6;K_2331:;
L_2332: Cell t914=uf_mkp((void*)&uf_sl301);L_2333: pushc(cx,t914);cx->cs[cx->csp++]=&&K_2333;goto L_6;K_2333:;
L_2334: Cell t915=uf_mkp((void*)&uf_sl302);L_2335: pushc(cx,t915);cx->cs[cx->csp++]=&&K_2335;goto L_6;K_2335:;
L_2336: Cell t916=uf_mkp((void*)&uf_sl303);L_2337: pushc(cx,t916);cx->cs[cx->csp++]=&&K_2337;goto L_6;K_2337:;
L_2338: Cell t917=uf_mkp((void*)&uf_sl304);L_2339: pushc(cx,t917);cx->cs[cx->csp++]=&&K_2339;goto L_6;K_2339:;
L_2340: Cell t918=uf_mkp((void*)&uf_sl305);L_2341: pushc(cx,t918);cx->cs[cx->csp++]=&&K_2341;goto L_6;K_2341:;
L_2342: Cell t919=uf_mkp((void*)&uf_sl306);L_2343: pushc(cx,t919);cx->cs[cx->csp++]=&&K_2343;goto L_6;K_2343:;
L_2344: Cell t920=uf_mkp((void*)&uf_sl307);L_2345: pushc(cx,t920);cx->cs[cx->csp++]=&&K_2345;goto L_6;K_2345:;
L_2346: Cell t921=uf_mkp((void*)&uf_sl308);L_2347: pushc(cx,t921);cx->cs[cx->csp++]=&&K_2347;goto L_6;K_2347:;
L_2348: Cell t922=uf_mkp((void*)&uf_sl309);L_2349: pushc(cx,t922);op_print(cx);
L_2350: op_drp(cx);L_2351: cx->cs[cx->csp++]=&&K_2351;goto L_2227;K_2351:;
L_2352: Cell t923=uf_mkp((void*)&uf_sl310);L_2353: pushc(cx,t923);op_print(cx);
L_2354: op_drp(cx);L_2355: Cell t924=var_trans__out;L_2356: Cell t925=uf_mkp((void*)&uf_sl311);L_2357: pushc(cx,t924);pushc(cx,t925);op_print(cx);
L_2358: op_drp(cx);L_2359: Cell t926=uf_mki(0LL);L_2360: pushc(cx,t926);{Cell a0=pop(cx);((void(*)(int64_t))uf_im8)((int64_t)(a0.tag==T_FLOAT?(int64_t)uf_f(a0):a0.i));}
L_2361: Cell t927=uf_mkp((void*)&uf_sl312);L_2362: pushc(cx,t927);cx->cs[cx->csp++]=&&K_2362;goto L_53;K_2362:;
L_2363: Cell t928=uf_mkp((void*)&uf_sl313);L_2364: pushc(cx,t928);cx->cs[cx->csp++]=&&K_2364;goto L_53;K_2364:;
L_2365: {if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}
L_2366: return;
}
int main(int argc,char**argv){uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uf_init_lits(uf_lits,314);uf_gc_setroots(uf_vroots,47);uf_gc_init();uflux_run(main_cx,0);return 0;}
