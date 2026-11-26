// Package contract defines the initial, GUI-independent data contracts.
// S00 deliberately contains no encoder, probe, scheduler or predictor implementation.
package contract

import "time"

type Codec string

const (
	H264 Codec = "h264"
	HEVC Codec = "hevc"
	AV1  Codec = "av1"
)

type HDRKind string

const (
	HDRUnknown HDRKind = "unknown"
	SDR        HDRKind = "sdr"
	HDRPQ      HDRKind = "pq"
	HDRHLG     HDRKind = "hlg"
)

// Unknown measurements are nil; zero is not a substitute for missing data.
type MediaInfo struct {
	Path                                                   string
	Duration                                               *time.Duration
	Width, Height                                          *int
	FPSNumerator, FPSDenominator                           *int64
	BitDepth                                               *int
	Codec                                                  string
	HDR                                                    HDRKind
	ColorPrimaries, ColorTransfer, ColorMatrix, ColorRange string
}

type JobRequest struct {
	InputPath   string
	TargetBytes int64
	OutputCodec Codec
	// Zero means no additional size cap. Never upscale.
	MaxWidth, MaxHeight int
}

type Estimate struct {
	Duration    *time.Duration
	OutputBytes *int64
	Confidence  string
	EvidenceIDs []string
}

type Decision struct {
	Route       string
	ReasonCodes []string
	Estimate    Estimate
}

type BenchmarkRecord struct {
	SchemaVersion                     int
	SampleID, SampleSHA256            string
	Encoder, EncoderVersion, Preset   string
	CPUModel, GPUModel, DriverVersion string
	Codec                             Codec
	Width, Height, BitDepth           int
	FPSNumerator, FPSDenominator      int64
	Frames                            int64
	Elapsed                           time.Duration
	OutputBytes                       int64
	// Quality metrics carry algorithm/version and value, if actually measured.
	Metrics map[string]float64
}
