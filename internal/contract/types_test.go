package contract

import (
	"encoding/json"
	"testing"
)

func TestLargeByteBudgetAndUnicodeRoundTrip(t *testing.T) {
	want := JobRequest{InputPath: `I:\-=Converter=-\小K 的鹅腿坟场 & (黑洞) ^ % !.avi`, TargetBytes: 12_000_000_001, OutputCodec: HEVC}
	b, err := json.Marshal(want)
	if err != nil {
		t.Fatal(err)
	}
	var got JobRequest
	if err := json.Unmarshal(b, &got); err != nil {
		t.Fatal(err)
	}
	if got != want {
		t.Fatalf("round trip: got %#v, want %#v", got, want)
	}
}

func TestUnknownEstimateRemainsUnknown(t *testing.T) {
	b, err := json.Marshal(Estimate{})
	if err != nil {
		t.Fatal(err)
	}
	var e Estimate
	if err := json.Unmarshal(b, &e); err != nil {
		t.Fatal(err)
	}
	if e.Duration != nil || e.OutputBytes != nil {
		t.Fatal("unknown turned into a numeric estimate")
	}
}
