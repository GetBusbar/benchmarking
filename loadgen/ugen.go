package main
import ("bufio";"bytes";"flag";"fmt";"io";"math";"net/http";"sort";"strings";"sync";"sync/atomic";"time")
type hdrs []string
func(h *hdrs)String()string{return strings.Join(*h,",")}
func(h *hdrs)Set(v string)error{*h=append(*h,v);return nil}
// pct sorts v IN PLACE and returns its q-quantile with the index clamped to the last element (so
// q=1.0 or a rounding overshoot can never index out of bounds). NOTE: the in-place sort mutates the
// shared slice; it is only race-free because every caller runs AFTER wg.Wait() on the single output
// goroutine. If output formatting is ever parallelised, copy v (or guard) before sorting.
func pct(v []float64,q float64)float64{if len(v)==0{return 0};sort.Float64s(v);i:=int(float64(len(v))*q);if i>=len(v){i=len(v)-1};return v[i]}
// isContent: a data frame that carries actual delta text — OpenAI `"content":"…"` or Anthropic
// `"text":"…"` — and not an empty-string placeholder (content_block_start carries `"text":""`).
// The emptiness check is scoped to the content/text KEYS themselves: the previous blanket `:""`
// scan silently discarded every frame from gateways that reserialize chunks with unrelated
// empty-string fields (e.g. TensorZero's `"system_fingerprint":""`), zeroing their delivered-frame
// counts - a harness bug that was published as a gateway streaming failure.
func isContent(l string)bool{
 return (strings.Contains(l,`"content":"`)&&!strings.Contains(l,`"content":""`))||
  (strings.Contains(l,`"text":"`)&&!strings.Contains(l,`"text":""`))
}
// rpsOver: requests/sec over the ACTUAL elapsed wall time (never the nominal -d). Isolated so the
// audit-M1 "divide by measured elapsed, not nominal dur" fix has a regression test: dividing extra
// tail successes (a request in flight when the deadline passes still completes and is counted) by a
// shorter nominal window over-counts, and the over-count scales with latency, inflating slower
// gateways. A non-positive elapsed falls back to dur so we never divide by zero.
func rpsOver(ok int64, elapsed float64, dur int) int64{
 if elapsed<=0{elapsed=float64(dur)}
 if elapsed<=0{return 0}
 return int64(float64(ok)/elapsed)
}
// buildRequest constructs the POST for one loadgen iteration. Isolated so the audit-L1 NewRequest
// ERROR path (a malformed URL must increment fail and continue, never panic on a nil req) is testable.
func buildRequest(url,body,auth,shape string,stream bool,extra []string)(*http.Request,error){
 req,err:=http.NewRequest("POST",url,bytes.NewReader([]byte(body)))
 if err!=nil{return nil,err}
 req.Header.Set("content-type","application/json");req.Header.Set("authorization","Bearer "+auth)
 if shape=="anthropic"{req.Header.Set("anthropic-version","2023-06-01");req.Header.Set("x-api-key",auth)}
 if stream{req.Header.Set("accept","text/event-stream")}
 for _,h:=range extra{if i:=strings.Index(h,":");i>0{req.Header.Set(strings.TrimSpace(h[:i]),strings.TrimSpace(h[i+1:]))}}
 return req,nil
}
func main(){
 url:=flag.String("url","","");model:=flag.String("model","gpt-4o-mini","");auth:=flag.String("auth","sk-dummy","")
 conc:=flag.Int("c",200,"");dur:=flag.Int("d",12,"");psize:=flag.Int("psize",0,"pad content to N bytes")
 stream:=flag.Bool("stream",false,"SSE mode: request stream:true, consume frames, report TTFT/gaps")
 shape:=flag.String("shape","openai","request shape: openai (default, unchanged) | anthropic (Messages body + x-api-key/anthropic-version headers)")
 rawbody:=flag.String("body","","verbatim request body for every request (matrix per-cell sweep: the exact ingress-dialect probe body); overrides -shape/-psize body construction. Headers still come from -auth/-H")
 expframes:=flag.Int("expframes",0,"expected content frames per stream (delivered%% denominator; 0=skip)")
 stallus:=flag.Int64("stallus",0,"per-stream stall threshold µs — any content-frame gap above it marks the stream stalled (0=off)")
 var extra hdrs; flag.Var(&extra,"H","extra request header 'Key: Value' (repeatable)"); flag.Parse()
 if *shape!="openai"&&*shape!="anthropic"{fmt.Println("unknown -shape (want openai|anthropic)");return}
 pad:=strings.Repeat("x",*psize)
 var ok,fail int64; var mu sync.Mutex; lat:=[]float64{}
 // stream-mode pools (µs): TTFT per stream, content-frame gaps across all streams
 var frames,done,stalled int64; ttfts:=[]float64{}; gaps:=[]float64{}
 deadline:=time.Now().Add(time.Duration(*dur)*time.Second)
 tr:=&http.Transport{MaxIdleConns:0,MaxIdleConnsPerHost:*conc,MaxConnsPerHost:*conc}
 to:=30*time.Second; if *stream{to=120*time.Second}
 cl:=&http.Client{Transport:tr,Timeout:to}
 start:=time.Now()
 var wg sync.WaitGroup
 for w:=0;w<*conc;w++{wg.Add(1);go func(id int){defer wg.Done();n:=0
  for time.Now().Before(deadline){n++
   sfield:="";if *stream{sfield=`,"stream":true`}
   var body []byte
   if *rawbody!=""{
    body=[]byte(*rawbody)
   }else if *shape=="anthropic"{ // Anthropic Messages: max_tokens is REQUIRED, so it leads the body
    body=[]byte(fmt.Sprintf(`{"model":"%s","max_tokens":64,"messages":[{"role":"user","content":"u-%d-%d-%s"}]%s}`,*model,id,n,pad,sfield))
   }else{
    body=[]byte(fmt.Sprintf(`{"model":"%s","messages":[{"role":"user","content":"u-%d-%d-%s"}],"max_tokens":16%s}`,*model,id,n,pad,sfield))
   }
   // anthropic shape sends BOTH auth carriers (x-api-key like the Anthropic SDK, plus the Bearer)
   // so a gateway honoring either accepts it; -H can still override either header. A NewRequest error
   // (malformed URL) increments fail and continues — never a nil-req panic (audit L1).
   st:=time.Now();req,rerr:=buildRequest(*url,string(body),*auth,*shape,*stream,extra);if rerr!=nil{atomic.AddInt64(&fail,1);continue}
   resp,err:=cl.Do(req);if err!=nil{atomic.AddInt64(&fail,1);continue}
   if !*stream{
    io.Copy(io.Discard,resp.Body);resp.Body.Close()
    // a latency percentile must reflect only SUCCESSFUL proxied requests: a non-200 (429/5xx/auth)
    // is a failure, not a fast request, so its round-trip time must NOT enter the percentile pool.
    if resp.StatusCode==200{
     atomic.AddInt64(&ok,1)
     ms:=float64(time.Since(st).Microseconds())/1000.0;mu.Lock();lat=append(lat,ms);mu.Unlock()
    }else{atomic.AddInt64(&fail,1)}
    continue}
   // SSE: read line by line, timestamp every content frame as it lands
   var ttft float64=-1;var prev time.Time;var nf int64;fin:=false;mygaps:=[]float64{};mymax:=0.0
   sc:=bufio.NewScanner(resp.Body);sc.Buffer(make([]byte,64*1024),1024*1024)
   for sc.Scan(){l:=sc.Text()
    if !strings.HasPrefix(l,"data:"){continue}
    p:=strings.TrimSpace(l[5:])
    if p=="[DONE]"||strings.Contains(p,`"message_stop"`){fin=true;continue}
    if !isContent(p){continue}
    now:=time.Now();nf++
    if ttft<0{ttft=float64(now.Sub(st).Microseconds())}else{g:=float64(now.Sub(prev).Microseconds());mygaps=append(mygaps,g);if g>mymax{mymax=g}}
    prev=now}
   resp.Body.Close()
   // a 200 that never framed (buffered/non-streaming gateway) is a failed stream, not a slow one
   if resp.StatusCode!=200||nf==0{atomic.AddInt64(&fail,1);continue}
   atomic.AddInt64(&ok,1);atomic.AddInt64(&frames,nf)
   if fin{atomic.AddInt64(&done,1)}
   if *stallus>0&&mymax>float64(*stallus){atomic.AddInt64(&stalled,1)}
   mu.Lock();ttfts=append(ttfts,ttft);gaps=append(gaps,mygaps...);mu.Unlock()}}(w)}
 wg.Wait()
 if *stream{
  elapsed:=time.Since(start).Seconds()
  started:=ok+fail
  del:=math.NaN()
  if *expframes>0&&started>0{del=float64(frames)/float64(started*int64(*expframes))}
  // streams= completed-or-not opened; fps = content frames per wall second over the whole window
  fmt.Printf("streams=%d complete=%d fail=%d stalled=%d frames=%d fps=%d delivered=%.4f ttft_p50us=%d ttft_p99us=%d gap_p50us=%d gap_p99us=%d\n",
    started,done,fail,stalled,frames,int64(float64(frames)/elapsed),del,
    int64(pct(ttfts,0.5)),int64(pct(ttfts,0.99)),int64(pct(gaps,0.5)),int64(pct(gaps,0.99)))
  return}
 sort.Float64s(lat);p:=func(q float64)float64{if len(lat)==0{return 0};i:=int(float64(len(lat))*q);if i>=len(lat){i=len(lat)-1};return lat[i]}
 // rps over the ACTUAL elapsed wall time, not the nominal -d: workers only re-check the deadline at
 // the top of the next iteration, so the request in flight when the deadline passes still completes
 // and is counted in `ok`. Dividing those extra successes by the nominal *dur (a shorter window than
 // they really spanned) over-counts, and the over-count scales with per-request latency - inflating
 // higher-latency / near-saturation gateways. Divide by real elapsed (as the stream path does).
 elapsed:=time.Since(start).Seconds()
 // ms for humans; us (integer microseconds) for sub-ms precision the perf suite parses.
 fmt.Printf("rps=%d fail=%d p50=%.2f p99=%.2f p50us=%d p99us=%d ok=%d\n",
   rpsOver(ok,elapsed,*dur),fail,p(0.5),p(0.99),int64(p(0.5)*1000),int64(p(0.99)*1000),ok)
}
