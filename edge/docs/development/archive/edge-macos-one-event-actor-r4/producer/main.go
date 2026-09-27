// Package main is a fresh source-review candidate for one bounded synthetic
// VehicleName actor. It is not the historical producer binary and carries no
// vehicle identity, certificate, endpoint, transaction or credential value.
package main

import (
	"crypto/tls"
	"crypto/x509"
	"flag"
	"fmt"
	"net/url"
	"os"
	"strings"
	"time"

	"github.com/gorilla/websocket"
	"github.com/teslamotors/fleet-telemetry/messages"
	"github.com/teslamotors/fleet-telemetry/protos"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"
)

const (
	helperIdentityMarker = "teslatlas-synthetic-vehicle-identity-v2"
	vehicleName          = "synthetic-b2"
	wireTopic            = "V"
	messageIDSuffix      = "-envelope"
)

func main() {
	endpoint := flag.String("endpoint", "", "fresh receiver wss endpoint")
	certificate := flag.String("cert", "", "owner-only client certificate")
	key := flag.String("key", "", "owner-only client key")
	serverCA := flag.String("server-ca", "", "receiver server CA")
	vin := flag.String("vin", "", "sealed VIN supplied by the wrapper")
	txid := flag.String("txid", "", "fresh transaction identifier")
	createdAtMS := flag.Int64("created-at-ms", 0, "one timestamp supplied by the wrapper")
	flag.Parse()

	// Keep the source/binary identity marker observable without embedding any
	// cohort-specific identity. The wrapper binds the executable hash before
	// reaching this point.
	fmt.Fprintf(os.Stderr, "actor-marker=%s\n", helperIdentityMarker)
	if flag.NArg() != 0 {
		fail(fmt.Errorf("unexpected positional arguments"))
	}
	if err := submit(*endpoint, *certificate, *key, *serverCA, *vin, *txid, *createdAtMS); err != nil {
		fail(err)
	}
}

func fail(err error) {
	fmt.Fprintf(os.Stderr, "synthetic vehicle failed: %v\n", err)
	os.Exit(1)
}

func submit(endpoint, certificatePath, keyPath, serverCAPath, vin, txid string, createdAtMS int64) error {
	if err := validateEndpoint(endpoint); err != nil {
		return err
	}
	if err := validateVIN(vin); err != nil {
		return err
	}
	if err := validateTransactionID(txid); err != nil {
		return err
	}
	if _, err := createdAtSeconds(createdAtMS); err != nil {
		return err
	}
	tlsConfig, err := loadClientTLSConfig(certificatePath, keyPath, serverCAPath, endpoint)
	if err != nil {
		return err
	}
	wireBytes, err := buildWireEnvelope(vin, txid, createdAtMS)
	if err != nil {
		return err
	}

	dialer := websocket.Dialer{
		TLSClientConfig:  tlsConfig,
		HandshakeTimeout: 10 * time.Second,
	}
	connection, _, err := dialer.Dial(endpoint, nil)
	if err != nil {
		return fmt.Errorf("dial receiver: %w", err)
	}
	defer connection.Close()

	if err := connection.WriteMessage(websocket.BinaryMessage, wireBytes); err != nil {
		return fmt.Errorf("write one envelope: %w", err)
	}
	messageType, ackBytes, err := connection.ReadMessage()
	if err != nil {
		return fmt.Errorf("read one ACK: %w", err)
	}
	if messageType != websocket.BinaryMessage {
		return fmt.Errorf("ACK message type %d is not binary", messageType)
	}
	if err := validateAck(ackBytes, txid); err != nil {
		return err
	}
	return nil
}

func loadClientTLSConfig(certificatePath, keyPath, serverCAPath, endpoint string) (*tls.Config, error) {
	certificate, err := tls.LoadX509KeyPair(certificatePath, keyPath)
	if err != nil {
		return nil, fmt.Errorf("load client certificate and key: %w", err)
	}
	caBytes, err := os.ReadFile(serverCAPath)
	if err != nil {
		return nil, fmt.Errorf("read server CA: %w", err)
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(caBytes) {
		return nil, fmt.Errorf("server CA contains no PEM certificate")
	}
	parsed, err := url.Parse(endpoint)
	if err != nil {
		return nil, fmt.Errorf("parse endpoint: %w", err)
	}
	return &tls.Config{
		MinVersion:   tls.VersionTLS12,
		Certificates: []tls.Certificate{certificate},
		RootCAs:      roots,
		ServerName:   parsed.Hostname(),
	}, nil
}

func buildPayload(vin string, createdAtMS int64) ([]byte, error) {
	if err := validateVIN(vin); err != nil {
		return nil, err
	}
	if _, err := createdAtSeconds(createdAtMS); err != nil {
		return nil, err
	}
	payload := &protos.Payload{
		Vin:       vin,
		CreatedAt: timestamppb.New(time.UnixMilli(createdAtMS)),
		Data: []*protos.Datum{{
			Key: protos.Field_VehicleName,
			Value: &protos.Value{Value: &protos.Value_StringValue{
				StringValue: vehicleName,
			}},
		}},
	}
	encoded, err := proto.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("marshal VehicleName payload: %w", err)
	}
	return encoded, nil
}

func buildWireEnvelope(vin, txid string, createdAtMS int64) ([]byte, error) {
	if err := validateTransactionID(txid); err != nil {
		return nil, err
	}
	createdAtSeconds, err := createdAtSeconds(createdAtMS)
	if err != nil {
		return nil, err
	}
	payload, err := buildPayload(vin, createdAtMS)
	if err != nil {
		return nil, err
	}
	stream := &messages.StreamMessage{
		MessageTopic: []byte(wireTopic),
		TXID:         []byte(txid),
		Payload:      payload,
		CreatedAt:    createdAtSeconds,
	}
	stream.SetIdentity("vehicle_device", strings.ReplaceAll(vin, ".", "-"))
	stream.SetMessageID([]byte(txid + messageIDSuffix))
	return stream.ToBytes()
}

func validateAck(encoded []byte, txid string) error {
	ack, err := messages.StreamAckMessageFromBytes(encoded)
	if err != nil {
		return fmt.Errorf("parse receiver ACK: %w", err)
	}
	if ack.Topic() != wireTopic {
		return fmt.Errorf("ACK topic %q does not match %q", ack.Topic(), wireTopic)
	}
	if string(ack.Txid()) != txid {
		return fmt.Errorf("ACK transaction does not match")
	}
	if len(ack.MessageID()) != 0 {
		return fmt.Errorf("ACK message id is unexpectedly nonempty")
	}
	return nil
}

func validateEndpoint(endpoint string) error {
	parsed, err := url.Parse(endpoint)
	if err != nil {
		return fmt.Errorf("invalid endpoint: %w", err)
	}
	if parsed.Scheme != "wss" || parsed.Hostname() == "" || parsed.User != nil {
		return fmt.Errorf("endpoint must be a wss URL without user info")
	}
	if parsed.RawQuery != "" || parsed.Fragment != "" || parsed.Path == "" {
		return fmt.Errorf("endpoint must have a path and no query or fragment")
	}
	return nil
}

func validateVIN(vin string) error {
	if len(vin) != 17 {
		return fmt.Errorf("VIN must have 17 ASCII characters")
	}
	for _, character := range vin {
		if character < '0' || character > 'Z' || (character > '9' && character < 'A') || strings.ContainsRune("IOQ", character) {
			return fmt.Errorf("VIN contains a forbidden character")
		}
	}
	return nil
}

func validateTransactionID(txid string) error {
	if txid == "" || len(txid) > 128 {
		return fmt.Errorf("transaction ID length is invalid")
	}
	for _, character := range txid {
		if !((character >= 'a' && character <= 'z') || (character >= 'A' && character <= 'Z') ||
			(character >= '0' && character <= '9') || strings.ContainsRune("-_.", character)) {
			return fmt.Errorf("transaction ID contains a forbidden character")
		}
	}
	return nil
}

func createdAtSeconds(createdAtMS int64) (uint32, error) {
	if createdAtMS <= 0 {
		return 0, fmt.Errorf("created-at-ms must be positive")
	}
	seconds := createdAtMS / 1000
	if seconds > int64(^uint32(0)) {
		return 0, fmt.Errorf("created-at-ms is outside the wire timestamp range")
	}
	return uint32(seconds), nil
}
